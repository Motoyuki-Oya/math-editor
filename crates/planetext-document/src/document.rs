//! 文書の本体。webview には見えている窓の行だけを渡し、編集・元に戻す・保存は
//! ここで行う。frontend は「行範囲の置き換え」を送ってくるだけで、文書の真実は
//! 常にこちら側にある。
//!
//! 本文はメモリに置かない。開くときに改行を 1 度だけ数え、行の場所は
//! [`crate::source::STRIDE`] 行ごとの行頭のバイト位置（間引きの索引）だけを控える。行の
//! 中身が要るときは、最寄りの索引へ seek してそこから読み流す。編集は
//! ピース列で表し、ディスクにそのまま残っている範囲と、編集で入った行だけ
//! を持つ。メモリは索引と編集量の分で済む。

use std::path::Path;
use std::sync::Arc;

use crate::edit_buffers::{EditBuffers, EditRange};
use crate::operation_log::{BulkOperation, Edit, OperationLog};
use crate::persistence::DraftDiff;
use crate::piece_tree::{Piece, PieceTree};
use crate::search::{convert_raw_hits, scan_encoded_range, ScanHit, SearchHitCache};
use crate::search_index::{BackgroundIndex, SearchIndex};
use crate::source::{BackgroundScan, FileEncoding, LineEnding, ScanIndex, Source};

#[derive(Clone, Copy)]
pub(crate) struct PendingSource {
    pub(crate) from: usize,
    pub(crate) len: usize,
    pub(crate) prefix_newlines: usize,
}

pub(crate) struct Document {
    pub(crate) source: Option<Source>,
    pieces: PieceTree,
    buffers: EditBuffers,
    /// すべてのピースの行数の合計。
    count: usize,
    log: OperationLog,
    encoding: FileEncoding,
    line_ending: LineEnding,
    pending_source: Option<PendingSource>,
    search_index: Option<SearchIndex>,
    background_index: Option<BackgroundIndex>,
    pending_redo_diffs: Vec<DraftDiff>,
    pub(crate) search_cache: Option<SearchHitCache>,
}

/// 元に戻す・やり直すの結果: 復元すべき控えと、行が変わった範囲の始まり。
/// frontend は `touched_from` から先の手元の行を捨てて取り寄せ直す。
pub(crate) struct Restored {
    pub(crate) state: String,
    pub(crate) touched_from: usize,
    pub(crate) line_count: usize,
}

impl Document {
    pub(crate) fn take_background_index(&mut self) -> Option<BackgroundIndex> {
        self.background_index.take()
    }

    pub(crate) fn is_scanning(&self) -> bool {
        self.pending_source.is_some()
    }

    pub(crate) fn set_pending_redo_diffs(&mut self, diffs: Vec<DraftDiff>) {
        self.pending_redo_diffs = diffs;
    }

    pub(crate) fn has_active_bulk(&self) -> bool {
        self.log.has_active_bulk()
    }

    pub(crate) fn is_same_source_path(&self, path: &str) -> bool {
        self.source
            .as_ref()
            .is_some_and(|source| source.path == Path::new(path))
    }

    pub(crate) fn take_source(&mut self) -> Option<Source> {
        self.source.take()
    }

    pub(crate) fn restore_source(&mut self, source: Option<Source>) {
        self.source = source;
    }

    pub(crate) fn reinitialize_after_save(&mut self, new_source: Source, is_small_file: bool) {
        let lines = self.count;
        let content_offset = new_source.content_offset as usize;
        let bytes = new_source.bytes as usize;
        self.source = Some(new_source);
        self.pieces = PieceTree::new(vec![Piece::Source {
            from: content_offset,
            len: bytes.saturating_sub(content_offset),
            newlines: lines.saturating_sub(1),
            starts_newline: false,
            ends_newline: false,
        }]);
        if !is_small_file {
            self.log.clear();
            self.buffers = EditBuffers::default();
        } else {
            self.log.mark_saved();
        }
        self.search_index = None;
    }

    pub(crate) fn search_index(&self) -> Option<&SearchIndex> {
        self.search_index.as_ref()
    }

    pub(crate) fn search_index_progress(&self) -> Option<(usize, usize)> {
        self.search_index.as_ref().map(|idx| idx.progress())
    }

    pub(crate) fn estimated_line_count(&self) -> usize {
        if let (Some(pending), Some(source)) = (self.pending_source, &self.source) {
            let pending_from = pending.from as u64;
            if pending_from > source.content_offset && source.bytes > source.content_offset {
                let scanned_bytes = (pending_from - source.content_offset) as u128;
                let total_bytes = (source.bytes - source.content_offset) as u128;
                ((self.count as u128 * total_bytes) / scanned_bytes.max(1)) as usize
            } else {
                self.count
            }
        } else {
            self.count
        }
    }

    fn source_pieces(source: &Source, count: usize) -> (Vec<Piece>, Option<PendingSource>) {
        let content_offset = source.content_offset as usize;
        let pending = source.pending_from.map(|from| PendingSource {
            from: from as usize,
            len: source.bytes as usize - from as usize,
            prefix_newlines: count,
        });
        let pieces = match pending {
            Some(pending) => vec![
                Piece::Source {
                    from: content_offset,
                    len: pending.from - content_offset,
                    newlines: count,
                    starts_newline: false,
                    ends_newline: true,
                },
                Piece::Source {
                    from: pending.from,
                    len: pending.len,
                    newlines: 0,
                    starts_newline: false,
                    ends_newline: true,
                },
            ],
            None => vec![Piece::Source {
                from: content_offset,
                len: source.bytes as usize - content_offset,
                newlines: count.saturating_sub(1),
                starts_newline: false,
                ends_newline: false,
            }],
        };
        (pieces, pending)
    }

    pub(crate) fn open(path: &str) -> Result<(Document, Option<BackgroundScan>), String> {
        Self::open_with_encoding(path, None)
    }

    pub(crate) fn open_with_encoding(
        path: &str,
        encoding: Option<FileEncoding>,
    ) -> Result<(Document, Option<BackgroundScan>), String> {
        let (source, scan) = Source::open_with_encoding(Path::new(path), encoding)?;
        let count = source.lines();
        let enc = source.encoding;
        let line_ending = source.line_ending;
        let (pieces, pending_source) = Self::source_pieces(&source, count);
        let (search_index, background_index) = {
            let content_bytes = (source.bytes.saturating_sub(source.content_offset)) as usize;
            if content_bytes >= crate::search_index::BIGRAM_INDEX_THRESHOLD {
                let index = SearchIndex::new(content_bytes, enc);
                let bg = BackgroundIndex {
                    path: Path::new(path).to_path_buf(),
                    content_offset: source.content_offset,
                    encoding: enc,
                    total_bytes: content_bytes,
                    state: index.state.clone(),
                };
                (Some(index), Some(bg))
            } else {
                (None, None)
            }
        };
        Ok((
            Document {
                pieces: PieceTree::new(pieces),
                buffers: EditBuffers::default(),
                count,
                encoding: enc,
                line_ending,
                source: Some(source),
                log: OperationLog::default(),
                pending_source,
                search_index,
                background_index,
                pending_redo_diffs: Vec::new(),
                search_cache: None,
            },
            scan,
        ))
    }

    #[allow(dead_code)]
    pub(crate) fn enable_search_index(&mut self) {
        let total_bytes = self.source.as_ref().map_or(0, |s| s.bytes as usize);
        let index = SearchIndex::new(total_bytes, self.encoding);
        if let Some(source) = self.source.as_mut() {
            let _ = index.ensure_all_blocks(source);
        }
        self.search_index = Some(index);
    }

    pub(crate) fn empty() -> Document {
        #[cfg(windows)]
        let line_ending = LineEnding::CrLf;
        #[cfg(not(windows))]
        let line_ending = LineEnding::Lf;
        Document {
            source: None,
            pieces: PieceTree::new(vec![Piece::Edit {
                from: 0,
                len: 0,
                newlines: 0,
                starts_newline: false,
                ends_newline: false,
                encoding: FileEncoding::Utf8,
                line_ending,
            }]),
            buffers: EditBuffers::default(),
            count: 1,
            log: OperationLog::default(),
            encoding: FileEncoding::Utf8,
            line_ending,
            pending_source: None,
            search_index: None,
            background_index: None,
            pending_redo_diffs: Vec::new(),
            search_cache: None,
        }
    }

    pub(crate) fn from_draft(lines: Vec<String>) -> Document {
        let mut doc = Self::empty();
        let mut lines: Vec<String> = lines
            .into_iter()
            .map(|line| line.trim_end_matches(['\r', '\n']).to_string())
            .collect();
        if lines.is_empty() {
            lines.push(String::new());
        }
        let (range, newlines) = doc
            .buffers
            .append_lines(&lines, doc.encoding, doc.line_ending);
        doc.pieces = PieceTree::new(vec![Piece::Edit {
            from: range.from,
            len: range.len,
            newlines,
            starts_newline: false,
            ends_newline: false,
            encoding: range.encoding,
            line_ending: range.line_ending,
        }]);
        doc.count = lines.len();
        doc.log.mark_dirty_without_history();
        doc
    }

    pub(crate) fn encoding(&self) -> FileEncoding {
        self.encoding
    }
    pub(crate) fn line_ending(&self) -> LineEnding {
        self.line_ending
    }
    pub(crate) fn set_encoding(&mut self, encoding: FileEncoding) {
        self.encoding = encoding;
    }
    pub(crate) fn set_line_ending(&mut self, line_ending: LineEnding) {
        self.line_ending = line_ending;
    }

    /// 指定した文字コードでファイルを開き直す。
    pub(crate) fn reopen_with_encoding(
        &mut self,
        encoding: FileEncoding,
    ) -> Result<Option<BackgroundScan>, String> {
        let Some(source) = self.source.as_ref() else {
            self.encoding = encoding;
            return Ok(None);
        };
        let path = source.path.clone();
        let (new_source, scan) = Source::open_with_encoding(&path, Some(encoding))?;
        let count = new_source.lines();
        self.count = count;
        self.encoding = encoding;
        self.line_ending = new_source.line_ending;
        let (pieces, pending_source) = Self::source_pieces(&new_source, count);
        self.pending_source = pending_source;
        self.pieces = PieceTree::new(pieces);
        self.buffers = EditBuffers::default();
        self.log.clear();
        let (search_index, bg_index) = {
            let content_bytes =
                (new_source.bytes.saturating_sub(new_source.content_offset)) as usize;
            if content_bytes >= crate::search_index::BIGRAM_INDEX_THRESHOLD {
                let index = SearchIndex::new(content_bytes, encoding);
                let bg = BackgroundIndex {
                    path: path.clone(),
                    content_offset: new_source.content_offset,
                    encoding,
                    total_bytes: content_bytes,
                    state: index.state.clone(),
                };
                (Some(index), Some(bg))
            } else {
                (None, None)
            }
        };
        self.search_index = search_index;
        self.background_index = bg_index;
        self.search_cache = None;
        self.source = Some(new_source);
        Ok(scan)
    }

    pub(crate) fn is_clean(&self) -> bool {
        self.log.is_clean()
    }
    pub(crate) fn line_count(&self) -> usize {
        self.count
    }
    pub(crate) fn bytes(&self) -> usize {
        self.pieces.byte_len
    }
    pub(crate) fn scan_index(&self) -> Option<Arc<ScanIndex>> {
        self.source.as_ref().map(|source| source.index.clone())
    }

    pub(crate) fn revision(&self) -> u64 {
        self.log.revision()
    }

    /// 未保存の操作ログ（unsaved_transactions）から、現在変更されている行番号のリストを厳密に導出する。
    pub(crate) fn modified_lines(&self) -> Vec<usize> {
        let mut modified = std::collections::BTreeSet::new();
        for tx in self.log.unsaved_transactions() {
            for edit in tx.edits() {
                let from_line = edit.from_line;
                let to_line = edit.from_line + edit.removed_lines;
                let end_line = edit.from_line + edit.inserted_lines;

                let removed = edit.removed_lines;
                let inserted = edit.inserted_lines;

                let mut next_modified = std::collections::BTreeSet::new();
                for &line in &modified {
                    if line < from_line {
                        next_modified.insert(line);
                    } else if line >= to_line {
                        let shifted = (line as isize + (inserted as isize - removed as isize))
                            .max(0) as usize;
                        next_modified.insert(shifted);
                    }
                }
                for l in from_line..end_line.max(from_line + 1) {
                    next_modified.insert(l);
                }
                modified = next_modified;
            }
        }
        modified.into_iter().collect()
    }

    /// 検索スレッドへ渡す読み取り専用の姿。ファイルカーソルは独立し、編集の
    /// ピースは開始時点の内容を複製するので、文書ロックを持たずに走査できる。
    pub(crate) fn search_snapshot(&self) -> Result<Document, String> {
        let mut snapshot = Document {
            source: self.source.as_ref().map(Source::search_copy).transpose()?,
            pieces: PieceTree::new(self.pieces.pieces()),
            buffers: self.buffers.clone(),
            count: self.count,
            log: self.log.clone(),
            encoding: self.encoding,
            line_ending: self.line_ending,
            pending_source: self.pending_source,
            search_index: self.search_index.clone(),
            background_index: None,
            pending_redo_diffs: Vec::new(),
            search_cache: self.search_cache.clone(),
        };
        snapshot.confirm_scan_if_done();
        Ok(snapshot)
    }

    pub(crate) fn line_column_to_bytes(&mut self, line: usize, col: usize) -> usize {
        let enc = self.encoding;
        if let Ok(lines) = self.read(line, 1) {
            if let Some(text) = lines.first() {
                return text.chars().take(col).map(|c| char_byte_len(c, enc)).sum();
            }
        }
        col
    }

    pub(crate) fn byte_offset_to_line_column(
        &mut self,
        target_byte: usize,
    ) -> Option<(usize, usize)> {
        let target_byte = target_byte.min(self.bytes());
        let mut low = 0;
        let mut high = self.count;
        while low + 1 < high {
            let mid = low + (high - low) / 2;
            let mid_byte = self.byte_offset_of_line(mid).unwrap_or(0);
            if mid_byte <= target_byte {
                low = mid;
            } else {
                high = mid;
            }
        }
        let line = low;
        let line_start_byte = self.byte_offset_of_line(line).unwrap_or(0);
        let col_bytes = target_byte.saturating_sub(line_start_byte);
        let enc = self.encoding;
        let col = if let Ok(lines) = self.read(line, 1) {
            if let Some(text) = lines.first() {
                let mut current_bytes = 0;
                let mut chars = 0;
                for c in text.chars() {
                    if current_bytes >= col_bytes {
                        break;
                    }
                    current_bytes += char_byte_len(c, enc);
                    chars += 1;
                }
                chars
            } else {
                col_bytes
            }
        } else {
            col_bytes
        };
        Some((line, col))
    }

    /// スナップショット時点で得られた検索結果を、現在のリビジョンにおける座標へ写像する。
    /// 編集と重なったヒットは無効化（除外）する。
    pub(crate) fn map_search_hits(
        &mut self,
        snapshot: &Document,
        hits: Vec<ScanHit>,
    ) -> Vec<ScanHit> {
        let snapshot_rev = snapshot.revision();
        if snapshot_rev == self.revision() {
            return hits;
        }
        let mut mapped = Vec::with_capacity(hits.len());
        for hit in hits {
            let mut snap_clone = snapshot.clone_for_query();
            let line_start_byte = snap_clone.byte_offset_of_line(hit.line).unwrap_or(0);
            let start_col_bytes = snap_clone.line_column_to_bytes(hit.line, hit.start);
            let end_col_bytes = snap_clone.line_column_to_bytes(hit.line, hit.end);
            let start_byte = line_start_byte + start_col_bytes;
            let end_byte = line_start_byte + end_col_bytes;
            if let Ok((new_start, new_end)) = self.log.map_range(snapshot_rev, start_byte, end_byte)
            {
                if let (Some((line1, col1)), Some((line2, col2))) = (
                    self.byte_offset_to_line_column(new_start),
                    self.byte_offset_to_line_column(new_end),
                ) {
                    if line1 == line2 {
                        mapped.push(ScanHit {
                            line: line1,
                            notation: hit.notation,
                            start: col1,
                            end: col2,
                        });
                    }
                }
            }
        }
        mapped
    }

    fn clone_for_query(&self) -> Document {
        Document {
            source: self.source.as_ref().and_then(|s| s.search_copy().ok()),
            pieces: PieceTree::new(self.pieces.pieces()),
            buffers: self.buffers.clone(),
            count: self.count,
            log: self.log.clone(),
            encoding: self.encoding,
            line_ending: self.line_ending,
            pending_source: self.pending_source,
            search_index: None,
            background_index: None,
            pending_redo_diffs: Vec::new(),
            search_cache: None,
        }
    }

    /// 走査完了後に呼ぶ。未走査だった元ファイル範囲がまだ残る場合だけ集約値を確定する。
    pub(crate) fn confirm_scan(&mut self) {
        let Some(pending) = self.pending_source.take() else {
            return;
        };
        let Some(source) = self.source.as_ref() else {
            return;
        };
        let exact_newlines = source
            .lines()
            .saturating_sub(1)
            .saturating_sub(pending.prefix_newlines);
        self.pieces
            .confirm_source_range(pending.from, pending.len, exact_newlines);
        self.count = self.pieces.line_count();
    }

    /// 保存された総行数を用いて、走査を待たずにピースツリーの行数を即時確定する。
    pub(crate) fn confirm_scan_with_total_lines(&mut self, total_lines: usize) {
        let Some(pending) = self.pending_source.take() else {
            return;
        };
        let exact_newlines = total_lines
            .saturating_sub(1)
            .saturating_sub(pending.prefix_newlines);
        self.pieces
            .confirm_source_range(pending.from, pending.len, exact_newlines);
        self.count = self.pieces.line_count();
    }

    /// 確定した総行数を返す。走査未完了（pending_source が残っている）の場合は None を返す。
    pub(crate) fn confirmed_line_count(&self) -> Option<usize> {
        if self.pending_source.is_some() {
            None
        } else {
            Some(self.count)
        }
    }

    /// バックグラウンド走査が完了しているか確認し、完了していれば即座に行数を確定する。
    pub(crate) fn confirm_scan_if_done(&mut self) {
        let scan_done = self
            .source
            .as_ref()
            .and_then(|source| source.index.status().ok().flatten())
            .is_some();
        if scan_done {
            self.confirm_scan();
        }
    }

    fn pending_source_index(&self) -> Option<usize> {
        let pending = self.pending_source?;
        self.pieces.source_range_index(pending.from, pending.len)
    }

    pub(crate) fn apply_bulk_rules(
        operations: &[(&BulkOperation, u64)],
        line_idx: usize,
        text: &str,
    ) -> String {
        if operations.is_empty() {
            return text.to_string();
        }
        let mut current = text.to_string();
        for (op, _) in operations {
            match op {
                BulkOperation::AllLines {
                    from_line,
                    to_line,
                    column,
                    delete,
                    insert,
                } => {
                    if (*from_line..*to_line).contains(&line_idx) {
                        let start = current
                            .char_indices()
                            .nth(*column)
                            .map_or(current.len(), |(i, _)| i);
                        let end = current
                            .char_indices()
                            .nth(column.saturating_add(*delete))
                            .map_or(current.len(), |(i, _)| i);
                        if start <= end {
                            current.replace_range(start..end, insert);
                        }
                    }
                }
                BulkOperation::ReplaceAll {
                    from_line,
                    to_line,
                    query,
                    replacement,
                    case_sensitive,
                    pattern,
                } => {
                    if (*from_line..*to_line).contains(&line_idx) && !query.is_empty() {
                        if *case_sensitive {
                            current = current.replace(query, replacement);
                        } else {
                            // 事前コンパイル済みの pattern を使用し、行ごとの RegexBuilder 重複コンパイルを完全に排除！
                            current = pattern
                                .replace_all(&current, replacement.as_str())
                                .into_owned();
                        }
                    }
                }
            }
        }
        current
    }

    pub(crate) fn apply_bulk_operation(
        &mut self,
        base_revision: u64,
        group: u64,
        bulk: BulkOperation,
        before: &str,
        after: &str,
    ) -> Result<usize, String> {
        self.pending_redo_diffs.clear();
        self.log.validate_base(base_revision)?;
        self.log
            .append_bulk_transaction(base_revision, group, bulk, before, after);
        Ok(self.count)
    }

    /// 文書の行 `from..from+count` に `f` を呼ぶ。ディスクの範囲は seek して
    /// 読み流し、編集で入った行はそのまま渡す。`f` が `false` で打ち切り。
    pub(crate) fn each_line(
        &mut self,
        from: usize,
        count: usize,
        f: &mut dyn FnMut(usize, &str) -> bool,
    ) -> Result<(), String> {
        let to = from.saturating_add(count).min(self.count);
        if from >= to {
            return Ok(());
        }
        let bulks = self.log.active_bulk_operations();
        let mut error = None;
        self.pieces
            .for_each_line_range(from, to, &mut |start, piece, skip, take| match piece {
                Piece::Edit {
                    from,
                    len,
                    newlines,
                    starts_newline,
                    ends_newline,
                    encoding,
                    line_ending,
                } => {
                    let leading = usize::from(starts_newline)
                        * EditBuffers::line_separator_len(encoding, line_ending);
                    self.buffers.for_each_line(
                        EditRange {
                            from: from + leading,
                            len: len - leading,
                            lines: newlines + usize::from(!ends_newline)
                                - usize::from(starts_newline),
                            encoding,
                            line_ending,
                        },
                        skip,
                        take,
                        &mut |i, text| {
                            let line_idx = start + skip + i;
                            if bulks.is_empty() {
                                f(line_idx, text)
                            } else {
                                let modified = Self::apply_bulk_rules(&bulks, line_idx, text);
                                f(line_idx, &modified)
                            }
                        },
                    )
                }
                Piece::Source { from, len, .. } => match self.source.as_mut() {
                    Some(source) => {
                        match source.for_each_range_line(from, len, skip, take, &mut |i, text| {
                            let line_idx = start + skip + i;
                            if bulks.is_empty() {
                                f(line_idx, text)
                            } else {
                                let modified = Self::apply_bulk_rules(&bulks, line_idx, text);
                                f(line_idx, &modified)
                            }
                        }) {
                            Ok(done) => done,
                            Err(e) => {
                                error = Some(e);
                                false
                            }
                        }
                    }
                    None => {
                        error = Some(
                            "文書ストアのディスク参照が失われました。開き直してください"
                                .to_string(),
                        );
                        false
                    }
                },
            });
        match error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    pub(crate) fn read(&mut self, from: usize, count: usize) -> Result<Vec<String>, String> {
        let mut lines = Vec::new();
        let mut total_bytes = 0;
        const MAX_READ_BYTES: usize = 20 * 1024 * 1024; // 20 MB 安全上限
        self.each_line(from, count, &mut |_, text| {
            total_bytes += text.len();
            lines.push(text.to_string());
            total_bytes < MAX_READ_BYTES
        })?;
        Ok(lines)
    }

    /// bulk 操作の動的評価を挟まず、ピースツリーに現在保持されている生の行を読み出す。
    pub(crate) fn each_raw_line(
        &mut self,
        from: usize,
        count: usize,
        f: &mut dyn FnMut(usize, &str) -> bool,
    ) -> Result<(), String> {
        let to = from.saturating_add(count).min(self.count);
        if from >= to {
            return Ok(());
        }
        let mut error = None;
        self.pieces
            .for_each_line_range(from, to, &mut |start, piece, skip, take| match piece {
                Piece::Edit {
                    from,
                    len,
                    newlines,
                    starts_newline,
                    ends_newline,
                    encoding,
                    line_ending,
                } => {
                    let leading = usize::from(starts_newline)
                        * EditBuffers::line_separator_len(encoding, line_ending);
                    self.buffers.for_each_line(
                        EditRange {
                            from: from + leading,
                            len: len - leading,
                            lines: newlines + usize::from(!ends_newline)
                                - usize::from(starts_newline),
                            encoding,
                            line_ending,
                        },
                        skip,
                        take,
                        &mut |i, text| {
                            let line_idx = start + skip + i;
                            f(line_idx, text)
                        },
                    )
                }
                Piece::Source { from, len, .. } => match self.source.as_mut() {
                    Some(source) => {
                        match source.for_each_range_line(from, len, skip, take, &mut |i, text| {
                            let line_idx = start + skip + i;
                            f(line_idx, text)
                        }) {
                            Ok(done) => done,
                            Err(e) => {
                                error = Some(e);
                                false
                            }
                        }
                    }
                    None => {
                        error = Some(
                            "文書ストアのディスク参照が失われました。開き直してください"
                                .to_string(),
                        );
                        false
                    }
                },
            });
        match error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    pub(crate) fn read_raw(&mut self, from: usize, count: usize) -> Result<Vec<String>, String> {
        let mut lines = Vec::new();
        self.each_raw_line(from, count, &mut |_, text| {
            lines.push(text.to_string());
            true
        })?;
        Ok(lines)
    }

    pub(crate) fn read_tail(&mut self, count: usize) -> Result<Vec<String>, String> {
        let scan_done = self
            .source
            .as_ref()
            .and_then(|source| source.index.status().ok().flatten())
            .is_some();
        let res = if scan_done {
            self.confirm_scan();
            self.read(self.count.saturating_sub(count), count)
        } else {
            self.source
                .as_mut()
                .ok_or_else(|| "末尾を読むファイルがありません".to_string())?
                .read_tail(count)
        };
        // 末尾アクセス時に末尾ブロックの索引をオンデマンド構築
        if let (Some(index), Some(source)) = (self.search_index.as_ref(), self.source.as_mut()) {
            let tail_byte =
                (source.bytes.saturating_sub(source.content_offset)).saturating_sub(1) as usize;
            let _ = index.ensure_block_at_byte(tail_byte, source);
        }
        res
    }

    /// `from..to` の行を `lines` に置き換え、操作ログに追記する。
    /// 直前のステップと同じ `group` なら 1 ステップにつながる。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn replace(
        &mut self,
        from: usize,
        to: usize,
        lines: Vec<String>,
        group: u64,
        before: &str,
        after: &str,
    ) -> Result<usize, String> {
        let base_rev = self.log.revision();
        self.replace_with_base(base_rev, from, to, lines, group, before, after)
    }

    /// 基準 revision を指定して置き換えを行う。
    /// 基準 revision が古い場合、ログから現在座標へ写像して適用する。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn replace_with_base(
        &mut self,
        base_revision: u64,
        from: usize,
        to: usize,
        lines: Vec<String>,
        group: u64,
        before: &str,
        after: &str,
    ) -> Result<usize, String> {
        self.pending_redo_diffs.clear();
        if self.log.has_active_bulk() {
            self.materialize_bulk_transactions()?;
        }
        self.log.validate_base(base_revision)?;
        let (actual_from, actual_to) = if base_revision == self.log.revision() {
            (from, to)
        } else {
            self.log.map_line_range(base_revision, from, to)?
        };

        if actual_from > actual_to || actual_to > self.count {
            return Err("置き換えの範囲が文書の外です".to_string());
        }
        let clean_lines: Vec<String> = lines
            .into_iter()
            .map(|line| line.trim_end_matches(['\r', '\n']).to_string())
            .collect();
        let removed_count = actual_to - actual_from;
        let removed_lines = self.read(actual_from, removed_count).unwrap_or_default();
        let edit = self.splice(actual_from, actual_to, clean_lines.clone())?;
        if let Some(index) = self.search_index.as_ref() {
            let byte_pos = self.pieces.byte_offset(actual_from);
            let removed_text = removed_lines.join("\n");
            let inserted_text = clean_lines.join("\n");
            index.splice_delta(&removed_text, &inserted_text, byte_pos);
        }
        self.log
            .append_or_merge_transaction(base_revision, group, edit, before, after);
        Ok(self.count)
    }

    /// 置き換えの本体。行 `from..to` を `lines` に置き換え、
    /// 取り除いた行を退避した `Edit` を返す。
    pub(crate) fn splice(
        &mut self,
        from: usize,
        to: usize,
        lines: Vec<String>,
    ) -> Result<Edit, String> {
        let removed_count = to - from;
        let removed_lines = self.read(from, removed_count)?;
        if removed_lines.len() != removed_count {
            return Err("置き換える範囲が大きすぎます".to_string());
        }
        let byte_from = self.byte_offset_of_line(from)?;
        let byte_to = self.byte_offset_of_line(to)?;
        let inserted_range = self.apply_raw_splice(from, to, &lines)?;
        let removed_range =
            self.log
                .append_deleted(&removed_lines, self.encoding, self.line_ending);
        Ok(Edit {
            from: byte_from,
            to: byte_to,
            from_line: from,
            removed: removed_range,
            inserted: inserted_range,
            removed_lines: removed_count,
            inserted_lines: lines.len(),
        })
    }

    /// 下書き復元専用の置換。ディスクから削除行を再読み出しせず、下書きに記録された
    /// `deleted_lines` を直接使用することで、走査完了を待たずに即時適用する。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn replace_with_deleted(
        &mut self,
        from: usize,
        to: usize,
        lines: Vec<String>,
        group: u64,
        before: &str,
        after: &str,
        deleted_lines: Vec<String>,
    ) -> Result<usize, String> {
        let base_revision = self.log.revision();
        if from > to || to > self.count {
            return Err("置き換えの範囲が文書の外です".to_string());
        }
        let clean_lines: Vec<String> = lines
            .into_iter()
            .map(|line| line.trim_end_matches(['\r', '\n']).to_string())
            .collect();
        let removed_count = to - from;
        let edit = if deleted_lines.len() == removed_count {
            self.splice_with_deleted(from, to, clean_lines.clone(), deleted_lines)?
        } else {
            // 旧フォーマット等で削除行が未記録の場合は通常 splice（フォールバック）
            self.splice(from, to, clean_lines.clone())?
        };
        self.log
            .append_or_merge_transaction(base_revision, group, edit, before, after);
        Ok(self.count)
    }

    pub(crate) fn splice_with_deleted(
        &mut self,
        from: usize,
        to: usize,
        lines: Vec<String>,
        deleted_lines: Vec<String>,
    ) -> Result<Edit, String> {
        let removed_count = to - from;
        let byte_from = self.byte_offset_of_line(from)?;
        let byte_to = self.byte_offset_of_line(to)?;
        let inserted_range = self.apply_raw_splice(from, to, &lines)?;
        let removed_range =
            self.log
                .append_deleted(&deleted_lines, self.encoding, self.line_ending);
        Ok(Edit {
            from: byte_from,
            to: byte_to,
            from_line: from,
            removed: removed_range,
            inserted: inserted_range,
            removed_lines: removed_count,
            inserted_lines: lines.len(),
        })
    }

    /// ピースツリーの指定行範囲 `from..to` を `lines` で置き換える低水準操作。
    fn apply_raw_splice(
        &mut self,
        from: usize,
        to: usize,
        lines: &[String],
    ) -> Result<EditRange, String> {
        let removed_count = to - from;
        let old_count = self.count;
        let pending_source_exists = self.pending_source_index().is_some();
        let a = self.split(from)?;
        let b = self.split(to)?;
        let inserted = lines.len();
        let starts_newline = from == to && from == old_count && from > 0 && !pending_source_exists;
        let ends_newline = to < old_count || (to == old_count && pending_source_exists);
        let (range, piece) = if !lines.is_empty() {
            let (range, newlines) = self.buffers.append_lines_with_boundaries(
                lines,
                self.encoding,
                self.line_ending,
                starts_newline,
                ends_newline,
            );
            (
                range,
                Some(Piece::Edit {
                    from: range.from,
                    len: range.len,
                    newlines,
                    starts_newline,
                    ends_newline,
                    encoding: range.encoding,
                    line_ending: range.line_ending,
                }),
            )
        } else {
            (
                EditRange {
                    from: 0,
                    len: 0,
                    lines: 0,
                    encoding: self.encoding,
                    line_ending: self.line_ending,
                },
                None,
            )
        };
        self.pieces.replace(a, b, piece.into_iter().collect());
        if inserted == 0 && to == old_count && from > 0 && !pending_source_exists {
            let separator_len = self.source.as_ref().map_or_else(
                || EditBuffers::line_separator_len(self.encoding, self.line_ending),
                |source| EditBuffers::line_separator_len(source.encoding, source.line_ending),
            );
            self.pieces.trim_trailing_newline(separator_len);
        }
        self.count = old_count - removed_count + inserted;
        if self.count == 0 {
            let (empty_range, _) =
                self.buffers
                    .append_lines(&[String::new()], self.encoding, self.line_ending);
            self.pieces.insert(
                0,
                Piece::Edit {
                    from: empty_range.from,
                    len: empty_range.len,
                    newlines: 0,
                    starts_newline: false,
                    ends_newline: false,
                    encoding: self.encoding,
                    line_ending: self.line_ending,
                },
            );
            self.count = 1;
        }
        debug_assert_eq!(self.count, self.pieces.line_count());
        if self.pending_source.is_none() {
            debug_assert_eq!(self.count.saturating_sub(1), self.pieces.newline_count);
        }
        Ok(range)
    }

    pub(crate) fn byte_offset_of_line(&mut self, line: usize) -> Result<usize, String> {
        let (index, start) = self.pieces.locate_line(line);
        let piece_start_byte = self.pieces.byte_offset_of_piece(index);
        let Some(piece) = self.pieces.piece(index).cloned() else {
            return Ok(piece_start_byte);
        };
        if line == start {
            return Ok(piece_start_byte);
        }
        let skip = line - start;
        let leading = usize::from(piece.starts_newline());
        let intra_byte = match &piece {
            Piece::Edit {
                from,
                len,
                newlines,
                starts_newline,
                ends_newline,
                encoding,
                line_ending,
            } => {
                let leading_bytes =
                    leading * EditBuffers::line_separator_len(*encoding, *line_ending);
                leading_bytes
                    + self.buffers.byte_offset_after_lines(
                        EditRange {
                            from: *from + leading_bytes,
                            len: *len - leading_bytes,
                            lines: *newlines + usize::from(!*ends_newline)
                                - usize::from(*starts_newline),
                            encoding: *encoding,
                            line_ending: *line_ending,
                        },
                        skip,
                    )
            }
            Piece::Source {
                from,
                len,
                newlines,
                ends_newline,
                ..
            } => {
                let source = self.source.as_mut().ok_or_else(|| {
                    "文書ストアのディスク参照が失われました。開き直してください".to_string()
                })?;
                Self::source_byte_offset_in_piece(
                    source,
                    *from,
                    *len,
                    *newlines,
                    *ends_newline,
                    skip,
                )?
            }
        };
        Ok(piece_start_byte + intra_byte)
    }

    /// ピース内の指定行数後のバイト位置を求める。末尾ピースかつ末尾付近なら EOF seek で即座に解決する。
    fn source_byte_offset_in_piece(
        source: &mut Source,
        from: usize,
        len: usize,
        newlines: usize,
        ends_newline: bool,
        skip: usize,
    ) -> Result<usize, String> {
        let piece_lines = newlines + usize::from(!ends_newline);
        let is_at_eof = from + len == source.bytes as usize;
        if is_at_eof && piece_lines >= skip {
            let from_end = piece_lines - skip;
            if from_end < crate::source::STRIDE * 2 {
                if let Ok(abs_pos) = source.byte_offset_from_end(from_end) {
                    if abs_pos >= from {
                        return Ok(abs_pos - from);
                    }
                }
            }
        }
        source.byte_offset_after_lines(from, len, skip)
    }

    /// ピース列を行 `line` の前で切り、その位置のピース番号を返す。
    fn split(&mut self, line: usize) -> Result<usize, String> {
        if line == self.count {
            if let Some(index) = self.pending_source_index() {
                return Ok(index);
            }
        }
        let (index, start) = self.pieces.locate_line(line);
        let Some(piece) = self.pieces.piece(index).cloned() else {
            return Ok(index);
        };
        let len = piece.lines();
        if line == start {
            return Ok(index);
        }
        if line < start + len {
            let skip = line - start;
            let leading = usize::from(piece.starts_newline());
            let byte = match &piece {
                Piece::Edit {
                    from,
                    len,
                    newlines,
                    starts_newline,
                    ends_newline,
                    encoding,
                    line_ending,
                } => {
                    let leading_bytes =
                        leading * EditBuffers::line_separator_len(*encoding, *line_ending);
                    leading_bytes
                        + self.buffers.byte_offset_after_lines(
                            EditRange {
                                from: *from + leading_bytes,
                                len: *len - leading_bytes,
                                lines: *newlines + usize::from(!*ends_newline)
                                    - usize::from(*starts_newline),
                                encoding: *encoding,
                                line_ending: *line_ending,
                            },
                            skip,
                        )
                }
                Piece::Source {
                    from,
                    len,
                    newlines,
                    ends_newline,
                    ..
                } => {
                    let source = self.source.as_mut().ok_or_else(|| {
                        "文書ストアのディスク参照が失われました。開き直してください".to_string()
                    })?;
                    Self::source_byte_offset_in_piece(
                        source,
                        *from,
                        *len,
                        *newlines,
                        *ends_newline,
                        skip,
                    )?
                }
            };
            let newlines = skip + leading;
            self.pieces.split_piece(index, byte, newlines, byte > 0);
            return Ok(index + 1);
        }
        Ok(index)
    }

    pub(crate) fn undo(&mut self) -> Result<Option<Restored>, String> {
        let Some(tx) = self.log.undo_pop() else {
            return Ok(None);
        };
        let edits = tx.edits().to_vec();
        let state = tx.before.clone();
        let mut touched_from = usize::MAX;
        for edit in edits.into_iter().rev() {
            touched_from = touched_from.min(edit.from_line);
            let restored_lines = self.log.read_deleted(edit.removed);
            let inserted_lines = self.buffers.read_lines(edit.inserted);
            self.apply_raw_splice(
                edit.from_line,
                edit.from_line + edit.inserted_lines,
                &restored_lines,
            )?;
            if let Some(index) = self.search_index.as_ref() {
                let byte_pos = edit.from;
                let restored_text = restored_lines.join("\n");
                let inserted_text = inserted_lines.join("\n");
                index.splice_delta(&inserted_text, &restored_text, byte_pos);
            }
        }
        Ok(Some(Restored {
            state,
            touched_from: if touched_from == usize::MAX {
                0
            } else {
                touched_from
            },
            line_count: self.count,
        }))
    }

    pub(crate) fn redo(&mut self) -> Result<Option<Restored>, String> {
        if let Some(tx) = self.log.redo_pop() {
            let edits = tx.edits().to_vec();
            let state = tx.after.clone();
            let mut touched_from = usize::MAX;
            for edit in edits.into_iter() {
                touched_from = touched_from.min(edit.from_line);
                let reapply_lines = self.buffers.read_lines(edit.inserted);
                let removed_lines = self.log.read_deleted(edit.removed);
                self.apply_raw_splice(
                    edit.from_line,
                    edit.from_line + edit.removed_lines,
                    &reapply_lines,
                )?;
                if let Some(index) = self.search_index.as_ref() {
                    let byte_pos = edit.from;
                    let removed_text = removed_lines.join("\n");
                    let reapplied_text = reapply_lines.join("\n");
                    index.splice_delta(&removed_text, &reapplied_text, byte_pos);
                }
            }
            return Ok(Some(Restored {
                state,
                touched_from: if touched_from == usize::MAX {
                    0
                } else {
                    touched_from
                },
                line_count: self.count,
            }));
        }
        if !self.pending_redo_diffs.is_empty() {
            let target_group = self.pending_redo_diffs[0].group;
            let mut state = String::new();
            let mut touched_from = usize::MAX;
            while !self.pending_redo_diffs.is_empty()
                && self.pending_redo_diffs[0].group == target_group
            {
                let diff = self.pending_redo_diffs.remove(0);
                let to_line = diff.from_line + diff.removed_lines;
                state = diff.after.clone();
                touched_from = touched_from.min(diff.from_line);
                self.replace_with_deleted(
                    diff.from_line,
                    to_line,
                    diff.lines,
                    diff.group,
                    &diff.before,
                    &diff.after,
                    diff.deleted_lines,
                )?;
            }
            return Ok(Some(Restored {
                state,
                touched_from: if touched_from == usize::MAX {
                    0
                } else {
                    touched_from
                },
                line_count: self.count,
            }));
        }
        Ok(None)
    }

    /// 一度に実体文字列として組み立てられるコピーの上限（10MB）。
    pub(crate) const MAX_ASSEMBLE_BYTES: usize = 10 * 1024 * 1024;

    /// 選択された範囲をひとつなぎのテキストにする。`first` / `last` は端の行の
    /// 切り出し（`None` なら行を丸ごと）。`overrides` の行は差し替えて使う。
    /// 実体化が 10MB を超える場合は安全のためにエラーを返す。
    pub(crate) fn assemble(
        &mut self,
        from: usize,
        first: Option<String>,
        to: usize,
        last: Option<String>,
        overrides: &std::collections::HashMap<usize, String>,
    ) -> Result<String, String> {
        if from > to || to >= self.count {
            return Err("コピーの範囲が文書の外です".to_string());
        }
        let mut out = String::new();
        let mut exceeded = false;
        self.each_line(from, to - from + 1, &mut |i, line| {
            if i > from {
                out.push('\n');
            }
            if i == from && first.is_some() {
                out.push_str(first.as_deref().unwrap_or_default());
            } else if i == to && last.is_some() {
                out.push_str(last.as_deref().unwrap_or_default());
            } else {
                out.push_str(overrides.get(&i).map(String::as_str).unwrap_or(line));
            }
            if out.len() > Self::MAX_ASSEMBLE_BYTES {
                exceeded = true;
                return false;
            }
            true
        })?;
        if exceeded {
            return Err("コピー範囲が大きすぎます（上限10MB）".to_string());
        }
        Ok(out)
    }

    /// 文書が消費している編集実体・操作ログ・差分キャッシュ・検索索引の総メモリ量（バイト）を返す。
    #[allow(dead_code)]
    pub(crate) fn memory_usage(&self) -> usize {
        let index_mem = self
            .search_index
            .as_ref()
            .map(|idx| idx.memory_usage())
            .unwrap_or(0);
        self.buffers.len() + self.log.memory_usage() + index_mem
    }

    /// トランザクションを等価な splice へ変換する。このまま active に残すと、
    /// 変換後の行へ同じ規則が再適用され（foo→foofoo の二重化）、Undo も
    /// 効かなくなる。変換後は通常の編集として Undo/Redo・下書き化できる。
    pub(crate) fn materialize_bulk_transactions(&mut self) -> Result<(), String> {
        use crate::operation_log::OperationKind;
        let targets: Vec<usize> = self.log.transactions[..self.log.head]
            .iter()
            .enumerate()
            .filter_map(|(i, tx)| matches!(tx.kind, OperationKind::Bulk(_)).then_some(i))
            .collect();
        for &index in targets.iter().rev() {
            let (base_revision, group, revision, bulk, before_text, after_text) = {
                let tx = &self.log.transactions[index];
                match &tx.kind {
                    OperationKind::Bulk(bulk) => (
                        tx.base_revision,
                        tx.group,
                        tx.revision,
                        bulk.clone(),
                        tx.before.clone(),
                        tx.after.clone(),
                    ),
                    OperationKind::Splice(_) => continue,
                }
            };
            let mut edits = Vec::new();
            match &bulk {
                crate::operation_log::BulkOperation::AllLines {
                    from_line, to_line, ..
                }
                | crate::operation_log::BulkOperation::ReplaceAll {
                    from_line, to_line, ..
                } => {
                    // 範囲内の各行を 1 編集として記録する。内容の変化有無で絞ると、
                    // たまたま元と同じ結果になった行が欠けて Undo で復元できない
                    // ため、位置ベースで全行を固める。後ろの行から置き換え、
                    // まだ置き換えていない行の座標を変えない。
                    for line_idx in (*from_line..*to_line).rev() {
                        if line_idx >= self.count {
                            break;
                        }
                        let original = self.read_raw(line_idx, 1)?;
                        let current = self.read(line_idx, 1)?;
                        edits.push(self.splice_with_deleted(
                            line_idx,
                            line_idx + 1,
                            current,
                            original,
                        )?);
                    }
                }
            }
            let tx = crate::operation_log::Transaction {
                group,
                base_revision,
                revision,
                kind: OperationKind::Splice(edits),
                before: before_text,
                after: after_text,
            };
            self.log.transactions[index] = tx;
        }
        Ok(())
    }

    /// 未保存のトランザクションから、発生した時系列順に操作ログ（差分）を抽出する。
    /// Undo された（Redo 可能な）トランザクションも含めて保持し、復元後に Redo できるようにする。
    pub(crate) fn collect_draft_diffs(&mut self) -> (Vec<DraftDiff>, usize) {
        let (txs, head_tx_offset) = self.log.all_unsaved_transactions();
        let mut diffs = Vec::new();
        let mut active_diffs_count = 0;
        for (tx_idx, tx) in txs.iter().enumerate() {
            let group = (tx_idx as u64) + 1;
            for edit in tx.edits() {
                let mut lines = Vec::with_capacity(edit.inserted_lines);
                self.buffers.for_each_line(
                    edit.inserted,
                    0,
                    edit.inserted_lines,
                    &mut |_, line| {
                        lines.push(line.to_string());
                        true
                    },
                );
                let deleted_lines = self.log.read_deleted(edit.removed);
                let fallback_pos = format!("{}.0-{}.0", edit.from_line, edit.from_line);
                let before = if tx.before.is_empty() {
                    fallback_pos.clone()
                } else {
                    tx.before.clone()
                };
                let after = if tx.after.is_empty() {
                    fallback_pos
                } else {
                    tx.after.clone()
                };
                diffs.push(DraftDiff {
                    group,
                    from_line: edit.from_line,
                    removed_lines: edit.removed_lines,
                    lines,
                    deleted_lines,
                    before,
                    after,
                });
                if tx_idx < head_tx_offset {
                    active_diffs_count += 1;
                }
            }
        }
        for pending in &self.pending_redo_diffs {
            diffs.push(pending.clone());
        }
        (diffs, active_diffs_count)
    }

    /// 通常の大小区別あり文字列検索。ディスクのピースはバイト範囲をまとめて
    /// memmem で探し、編集で入った行も同じ結果形式へ合わせる。
    pub(crate) fn scan_literal(
        &mut self,
        query: &str,
        case_sensitive: bool,
        marker: char,
        from: usize,
        count: usize,
        limit: usize,
    ) -> Result<(Vec<ScanHit>, usize), String> {
        let to = from.saturating_add(count).min(self.count);
        if from >= to || limit == 0 {
            return Ok((Vec::new(), from));
        }
        let query_characters = query.chars().count();
        let mmap = self.source.as_ref().and_then(|s| s.mmap().ok());
        let mut hits = Vec::new();
        let mut scanned_to = from;
        let mut error = None;
        self.pieces
            .for_each_line_range(from, to, &mut |piece_line, piece, skip, take| {
                if hits.len() >= limit || error.is_some() {
                    return false;
                }
                let result: Result<(Vec<ScanHit>, usize), String> = match piece {
                    Piece::Source { from, len, .. } => {
                        let Some(source) = self.source.as_mut() else {
                            error = Some(
                                "文書ストアのディスク参照が失われました。開き直してください"
                                    .to_string(),
                            );
                            return false;
                        };
                        (|| {
                            let (range_from, range_to) =
                                source.byte_range_for_lines(from, len, skip, take)?;
                            let encoding = source.encoding;
                            let delimiter = source.delimiter();
                            let encoded_query = encoding.encode_str(query);
                            let encoded_marker = encoding.encode_str(&marker.to_string());
                            let (raw, scanned) = if let Some(ref mmap) = mmap {
                                scan_encoded_range(
                                    range_to - range_from,
                                    take,
                                    encoding,
                                    &delimiter,
                                    &encoded_query,
                                    &encoded_marker,
                                    case_sensitive,
                                    limit - hits.len(),
                                    |offset, size| {
                                        let start = range_from + offset;
                                        let end = (start + size).min(mmap.len());
                                        if start <= mmap.len() {
                                            Ok(mmap[start..end].to_vec())
                                        } else {
                                            Err("mmap 範囲外アクセス".to_string())
                                        }
                                    },
                                )?
                            } else {
                                scan_encoded_range(
                                    range_to - range_from,
                                    take,
                                    encoding,
                                    &delimiter,
                                    &encoded_query,
                                    &encoded_marker,
                                    case_sensitive,
                                    limit - hits.len(),
                                    |offset, size| source.read_byte_range(range_from + offset, size),
                                )?
                            };
                            let converted = if let Some(ref mmap) = mmap {
                                convert_raw_hits(
                                    raw,
                                    encoding,
                                    piece_line + skip,
                                    query_characters,
                                    |offset, size| {
                                        let start = range_from + offset;
                                        let end = (start + size).min(mmap.len());
                                        if start <= mmap.len() {
                                            Ok(mmap[start..end].to_vec())
                                        } else {
                                            Err("mmap 範囲外アクセス".to_string())
                                        }
                                    },
                                )?
                            } else {
                                convert_raw_hits(
                                    raw,
                                    encoding,
                                    piece_line + skip,
                                    query_characters,
                                    |offset, size| source.read_byte_range(range_from + offset, size),
                                )?
                            };
                            Ok((converted, scanned))
                        })()
                    }
                    Piece::Edit {
                        from,
                        len,
                        newlines,
                        starts_newline,
                        ends_newline,
                        encoding,
                        line_ending,
                        ..
                    } => {
                        let leading = usize::from(starts_newline)
                            * crate::edit_buffers::EditBuffers::line_separator_len(
                                encoding,
                                line_ending,
                            );
                        let range = EditRange {
                            from: from + leading,
                            len: len - leading,
                            lines: newlines + usize::from(!ends_newline)
                                - usize::from(starts_newline),
                            encoding,
                            line_ending,
                        };
                        let bytes = self.buffers.bytes(range);
                        let range_start = self.buffers.byte_offset_after_lines(range, skip);
                        let range_end = self
                            .buffers
                            .byte_offset_after_lines(range, skip.saturating_add(take));
                        let selected = &bytes[range_start..range_end];
                        let delimiter = encoding.encode_str(match line_ending {
                            crate::source::LineEnding::Cr => "\r",
                            _ => "\n",
                        });
                        let encoded_query = encoding.encode_str(query);
                        let encoded_marker = encoding.encode_str(&marker.to_string());
                        scan_encoded_range(
                            selected.len(),
                            take,
                            encoding,
                            &delimiter,
                            &encoded_query,
                            &encoded_marker,
                            case_sensitive,
                            limit - hits.len(),
                            |offset, size| Ok(selected[offset..offset + size].to_vec()),
                        )
                        .and_then(|(raw, scanned)| {
                            let converted = convert_raw_hits(
                                raw,
                                encoding,
                                piece_line + skip,
                                query_characters,
                                |offset, size| Ok(selected[offset..offset + size].to_vec()),
                            )?;
                            Ok((converted, scanned))
                        })
                    }
                };
                match result {
                    Ok((mut piece_hits, scanned)) => {
                        hits.append(&mut piece_hits);
                        scanned_to = piece_line + skip + scanned;
                        hits.len() < limit
                    }
                    Err(message) => {
                        error = Some(message);
                        false
                    }
                }
            });
        if let Some(error) = error {
            Err(error)
        } else {
            Ok((hits, scanned_to))
        }
    }
}

fn char_byte_len(c: char, encoding: FileEncoding) -> usize {
    match encoding {
        FileEncoding::Utf8 | FileEncoding::Utf8Bom => c.len_utf8(),
        FileEncoding::Utf16Le | FileEncoding::Utf16Be => {
            if c.len_utf16() == 1 {
                2
            } else {
                4
            }
        }
        _ => encoding.encode_str(&c.to_string()).len(),
    }
}

#[cfg(test)]
impl Document {
    pub(crate) fn source_content_offset_for_test(&self) -> Option<u64> {
        self.source.as_ref().map(|s| s.content_offset)
    }

    pub(crate) fn pieces_line_count_for_test(&self) -> usize {
        self.pieces.line_count()
    }

    pub(crate) fn pieces_newline_count_for_test(&self) -> usize {
        self.pieces.newline_count
    }

    pub(crate) fn pieces_byte_len_for_test(&self) -> usize {
        self.pieces.byte_len
    }

    pub(crate) fn pieces_byte_offset_for_test(&self, line: usize) -> usize {
        self.pieces.byte_offset(line)
    }

    pub(crate) fn buffers_len_for_test(&self) -> usize {
        self.buffers.len()
    }

    pub(crate) fn log_transactions_len_for_test(&self) -> usize {
        self.log.transactions.len()
    }

    pub(crate) fn log_head_for_test(&self) -> usize {
        self.log.head
    }

    pub(crate) fn log_validate_base_for_test(&self, base_rev: u64) -> Result<(), String> {
        self.log.validate_base(base_rev)
    }

    pub(crate) fn log_first_tx_removed_lines_for_test(&self) -> usize {
        self.log.transactions[0].edits()[0].removed.lines
    }

    pub(crate) fn simulate_pending_source_for_test(
        &mut self,
        initial_bytes: usize,
        initial_lines: usize,
        total_bytes: usize,
    ) {
        self.pieces = PieceTree::new(vec![
            Piece::Source {
                from: 0,
                len: initial_bytes,
                newlines: initial_lines - 1,
                starts_newline: false,
                ends_newline: true,
            },
            Piece::Source {
                from: initial_bytes,
                len: total_bytes - initial_bytes,
                newlines: 0,
                starts_newline: true,
                ends_newline: false,
            },
        ]);
        self.count = initial_lines;
        self.pending_source = Some(PendingSource {
            from: initial_bytes,
            len: total_bytes - initial_bytes,
            prefix_newlines: initial_lines - 1,
        });
    }

    pub(crate) fn source_sparse_mark_for_test(&self, index: usize) -> Option<u64> {
        self.source
            .as_ref()
            .map(|s| s.index.state.lock().unwrap().marks[index])
    }

    pub(crate) fn source_bytes_for_test(&self) -> Option<u64> {
        self.source.as_ref().map(|s| s.bytes)
    }

    pub(crate) fn simulate_scan_done_for_test(&mut self, lines: usize) {
        if let Some(source) = &self.source {
            let mut state = source.index.state.lock().unwrap();
            state.done = true;
            state.lines = lines;
        }
    }
}



#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{FileEncoding, LineEnding};
    use crate::test_utils::*;

    #[test]
    fn opening_indexes_lines_without_holding_the_contents() {
        let (mut doc, path) = disk_doc("open", &["ab", "", "cd"]);
        assert_eq!(doc.line_count(), 3);
        assert_eq!(all(&mut doc), vec!["ab", "", "cd"]);
        assert_eq!(doc.read(2, 1).unwrap(), vec!["cd"]);
        std::fs::remove_file(path).ok();
    }


    #[test]
    fn replacing_lines_and_undoing_restores_the_document() {
        let (mut doc, path) = disk_doc("undo", &["a", "b", "c"]);
        doc.replace(1, 2, vec!["X".into(), "Y".into()], 1, "before", "after")
            .unwrap();
        assert_eq!(all(&mut doc), vec!["a", "X", "Y", "c"]);
        let undone = doc.undo().unwrap().unwrap();
        assert_eq!(undone.state, "before");
        assert_eq!(undone.touched_from, 1);
        assert_eq!(all(&mut doc), vec!["a", "b", "c"]);
        let redone = doc.redo().unwrap().unwrap();
        assert_eq!(redone.state, "after");
        assert_eq!(all(&mut doc), vec!["a", "X", "Y", "c"]);
        std::fs::remove_file(path).ok();
    }


    #[test]
    fn no_op_edit_and_undo_keep_zero_removed_lines() {
        let (mut doc, path) = disk_doc("no-op", &["a", "b"]);
        doc.replace(1, 1, Vec::new(), 1, "before", "after").unwrap();

        assert_eq!(doc.log_first_tx_removed_lines_for_test(), 0);
        assert_eq!(all(&mut doc), vec!["a", "b"]);
        let undone = doc.undo().unwrap().unwrap();
        assert_eq!(undone.state, "before");
        assert_eq!(all(&mut doc), vec!["a", "b"]);
        assert_eq!(doc.redo().unwrap().unwrap().state, "after");
        assert_eq!(all(&mut doc), vec!["a", "b"]);
        std::fs::remove_file(path).ok();
    }


    #[test]
    fn steps_in_the_same_group_undo_together() {
        let (mut doc, path) = disk_doc("group", &["a", "b", "c"]);
        // 「すべて置換」のように、複数の置き換えが 1 つのグループで届く。
        doc.replace(2, 3, vec!["C".into()], 7, "start", "mid")
            .unwrap();
        doc.replace(0, 1, vec!["A".into()], 7, "ignored", "end")
            .unwrap();
        assert_eq!(all(&mut doc), vec!["A", "b", "C"]);
        let undone = doc.undo().unwrap().unwrap();
        assert_eq!(all(&mut doc), vec!["a", "b", "c"]);
        assert_eq!(undone.state, "start");
        assert_eq!(undone.touched_from, 0);
        assert!(doc.undo().unwrap().is_none());
        let redone = doc.redo().unwrap().unwrap();
        assert_eq!(all(&mut doc), vec!["A", "b", "C"]);
        assert_eq!(redone.state, "end");
        std::fs::remove_file(path).ok();
    }


    #[test]
    fn different_groups_undo_one_at_a_time() {
        let mut doc = Document::empty();
        doc.replace(0, 1, vec!["b".into()], 1, "s1", "e1").unwrap();
        doc.replace(0, 1, vec!["c".into()], 2, "s2", "e2").unwrap();
        doc.undo().unwrap();
        assert_eq!(all(&mut doc), vec!["b"]);
        doc.undo().unwrap();
        assert_eq!(all(&mut doc), vec![""]);
    }


    #[test]
    fn a_new_edit_clears_the_redo_branch() {
        let mut doc = Document::empty();
        doc.replace(0, 1, vec!["b".into()], 1, "", "").unwrap();
        doc.undo().unwrap();
        doc.replace(0, 1, vec!["c".into()], 2, "", "").unwrap();
        assert!(doc.redo().unwrap().is_none());
        assert_eq!(all(&mut doc), vec!["c"]);
    }


    #[test]
    fn multiple_sequential_edits_in_one_group_undo_and_redo_correctly() {
        let mut doc = Document::empty();
        // 文字入力のように同一グループで順次行が置き換わる
        doc.replace(0, 1, vec!["a".into()], 1, "0.0-0.0", "0.1-0.1")
            .unwrap();
        doc.replace(0, 1, vec!["ab".into()], 1, "", "0.2-0.2")
            .unwrap();
        doc.replace(0, 1, vec!["abc".into()], 1, "", "0.3-0.3")
            .unwrap();
        assert_eq!(all(&mut doc), vec!["abc"]);

        // Undo 実行: 最初の "" (空) に戻る
        let undone = doc.undo().unwrap().unwrap();
        assert_eq!(undone.state, "0.0-0.0");
        assert_eq!(all(&mut doc), vec![""]);

        // Redo 実行: 最終状態 "abc" に正しく戻る（中間の "a" や "ab" に巻き戻らない）
        let redone = doc.redo().unwrap().unwrap();
        assert_eq!(redone.state, "0.3-0.3");
        assert_eq!(all(&mut doc), vec!["abc"]);

        // 再度 Undo 実行: 再び空に戻る
        let undone2 = doc.undo().unwrap().unwrap();
        assert_eq!(undone2.state, "0.0-0.0");
        assert_eq!(all(&mut doc), vec![""]);
    }


    #[test]
    fn out_of_range_replacements_are_rejected() {
        let mut doc = Document::empty();
        assert!(doc.replace(0, 2, vec![], 1, "", "").is_err());
        assert!(doc.replace(1, 0, vec![], 1, "", "").is_err());
    }


    #[test]
    fn assembling_a_range_uses_edges_and_overrides() {
        let (mut doc, path) = disk_doc("assemble", &["aa", "bb", "cc", "dd"]);
        let overrides = std::collections::HashMap::from([(2usize, "CC".to_string())]);
        assert_eq!(
            doc.assemble(0, Some("a".into()), 3, Some("d".into()), &overrides)
                .unwrap(),
            "a\nbb\nCC\nd"
        );
        assert_eq!(
            doc.assemble(1, None, 1, None, &Default::default()).unwrap(),
            "bb"
        );
        assert!(doc.assemble(0, None, 4, None, &Default::default()).is_err());
        std::fs::remove_file(path).ok();
    }


    /// 【回帰防止テスト】
    /// assemble の実体化が MAX_ASSEMBLE_BYTES（10MB）を超える場合、
    /// 安全のためにエラーを返し、巨大なヒープ確保によるプロセス圧迫を防ぐ。
    #[test]
    fn assemble_enforces_max_bytes_limit() {
        let large_line = "A".repeat(1024 * 1024); // 1MB の行
        let lines: Vec<String> = vec![large_line.clone(); 12]; // 12MB 分
        let lines_ref: Vec<&str> = lines.iter().map(String::as_str).collect();
        let (mut doc, path) = disk_doc("assemble-limit", &lines_ref);

        let overrides = std::collections::HashMap::default();
        let res = doc.assemble(0, None, 11, None, &overrides);
        assert!(res.is_err(), "10MB を超える assemble はエラーを返すこと");
        assert!(
            res.unwrap_err().contains("上限10MB"),
            "エラーメッセージに上限情報が含まれること"
        );
        std::fs::remove_file(path).ok();
    }


    /// 【回帰防止テスト】
    /// 編集バッファと操作ログのメモリ使用量が memory_usage() で正しく追跡されることを保証する。
    #[test]
    fn document_memory_usage_tracks_edits() {
        let (mut doc, path) = disk_doc("memory-tracking", &["hello", "world"]);
        let initial_memory = doc.memory_usage();

        doc.replace(1, 2, vec!["new content line".into()], 1, "", "")
            .unwrap();
        let edited_memory = doc.memory_usage();
        assert!(
            edited_memory > initial_memory,
            "編集によりメモリ追跡値が増加すること"
        );
        std::fs::remove_file(path).ok();
    }


    #[test]
    fn trailing_newline_final_empty_line_edits_target_the_right_range() {
        for line_ending in [LineEnding::Lf, LineEnding::CrLf] {
            for (operation, inserted, edited) in [
                ("edit", vec!["edited"], vec!["head", "edited"]),
                (
                    "replace",
                    vec!["replacement one", "replacement two"],
                    vec!["head", "replacement one", "replacement two"],
                ),
                ("delete", Vec::new(), vec!["head"]),
            ] {
                let name = format!("trailing-{line_ending:?}-{operation}");
                let (mut doc, path) =
                    encoded_disk_doc(&name, &["head"], FileEncoding::Utf8, line_ending, true);
                assert_encoded_document_state(&mut doc, &["head", ""]);

                doc.replace(
                    1,
                    2,
                    inserted.into_iter().map(str::to_string).collect(),
                    1,
                    "before",
                    "after",
                )
                .unwrap();
                assert_encoded_document_state(&mut doc, &edited);

                let undone = doc.undo().unwrap().unwrap();
                assert_eq!(undone.state, "before");
                assert_encoded_document_state(&mut doc, &["head", ""]);
                let redone = doc.redo().unwrap().unwrap();
                assert_eq!(redone.state, "after");
                assert_encoded_document_state(&mut doc, &edited);
                std::fs::remove_file(path).ok();
            }
        }
    }


    #[test]
    fn edited_separators_match_document_line_ending_and_encoding() {
        for (encoding, line_ending) in [
            (FileEncoding::Utf8, LineEnding::Lf),
            (FileEncoding::Utf8, LineEnding::CrLf),
            (FileEncoding::Utf8, LineEnding::Cr),
            (FileEncoding::Utf16Le, LineEnding::CrLf),
            (FileEncoding::Utf16Be, LineEnding::Cr),
        ] {
            let name = format!("separator-{encoding:?}-{line_ending:?}");
            let (mut doc, path) =
                encoded_disk_doc(&name, &["head", "tail"], encoding, line_ending, false);
            doc.replace(1, 1, vec!["左".into(), "右".into()], 1, "before", "after")
                .unwrap();
            assert_encoded_document_state(&mut doc, &["head", "左", "右", "tail"]);

            let saved = format!("{path}.saved");
            doc.save(&saved).unwrap();
            let expected_text = ["head", "左", "右", "tail"]
                .join(std::str::from_utf8(line_ending.as_bytes()).unwrap());
            let expected_bytes = match encoding {
                FileEncoding::Utf16Le | FileEncoding::Utf16Be => {
                    utf16_file_bytes(&expected_text, encoding)
                }
                _ => encoded_text(&expected_text, encoding),
            };
            assert_eq!(std::fs::read(&saved).unwrap(), expected_bytes);

            assert_eq!(doc.undo().unwrap().unwrap().state, "before");
            assert_encoded_document_state(&mut doc, &["head", "tail"]);
            assert_eq!(doc.redo().unwrap().unwrap().state, "after");
            assert_encoded_document_state(&mut doc, &["head", "左", "右", "tail"]);
            std::fs::remove_file(path).ok();
            std::fs::remove_file(saved).ok();
        }
    }


    #[test]
    fn boundary_separators_survive_insertions_and_replacements() {
        for (name, from, to, inserted, expected) in [
            (
                "replace-start",
                0,
                1,
                vec!["A", "AA"],
                vec!["A", "AA", "b", "c"],
            ),
            (
                "replace-middle",
                1,
                2,
                vec!["B", "BB"],
                vec!["a", "B", "BB", "c"],
            ),
            (
                "replace-eof",
                2,
                3,
                vec!["C", "CC"],
                vec!["a", "b", "C", "CC"],
            ),
            (
                "insert-start",
                0,
                0,
                vec!["H", "HH"],
                vec!["H", "HH", "a", "b", "c"],
            ),
            (
                "insert-middle",
                1,
                1,
                vec!["M", "MM"],
                vec!["a", "M", "MM", "b", "c"],
            ),
            (
                "insert-eof",
                3,
                3,
                vec!["T", "TT"],
                vec!["a", "b", "c", "T", "TT"],
            ),
        ] {
            let (mut doc, path) = disk_doc(name, &["a", "b", "c"]);
            doc.replace(
                from,
                to,
                inserted.into_iter().map(str::to_string).collect(),
                1,
                "",
                "",
            )
            .unwrap();
            let expected: Vec<String> = expected.into_iter().map(str::to_string).collect();
            assert_document_state(&mut doc, &expected);
            std::fs::remove_file(path).ok();
        }
    }


    #[test]
    fn document_boundaries_stay_consistent_beyond_node_capacity() {
        let (mut doc, path) = disk_doc("many-pieces", &["base"]);
        let mut expected = vec!["base".to_string()];
        for i in 0..24 {
            let line = format!("line-{i}");
            let at = doc.line_count();
            doc.replace(at, at, vec![line.clone()], i + 1, "", "")
                .unwrap();
            expected.push(line);
        }
        doc.replace(
            9,
            15,
            vec!["middle-a".into(), "middle-b".into(), "middle-c".into()],
            100,
            "",
            "",
        )
        .unwrap();
        expected.splice(
            9..15,
            ["middle-a", "middle-b", "middle-c"].map(str::to_string),
        );
        assert_document_state(&mut doc, &expected);
        std::fs::remove_file(path).ok();
    }


    #[test]
    fn oversized_delete_is_rejected_without_mutating_the_document() {
        let mut doc = Document::empty();
        let line = "a".repeat(5 * 1024 * 1024);
        doc.replace(
            0,
            1,
            vec![line.clone(), line.clone(), line.clone(), line.clone(), line],
            1,
            "before",
            "after",
        )
        .unwrap();
        let undo_len = doc.log_head_for_test();

        assert!(doc
            .replace(0, 5, Vec::new(), 2, "before delete", "after delete")
            .is_err());
        assert_eq!(doc.line_count(), 5);
        assert_eq!(doc.read(4, 1).unwrap().len(), 1);
        assert_eq!(doc.log_head_for_test(), undo_len);
    }


    #[test]
    fn replace_lines_with_base_revision_maps_old_coordinates() {
        let (mut doc, path) = disk_doc("base-rev-map", &["line0", "line1", "line2", "line3"]);
        let base_rev = doc.revision();

        // 別の編集が先頭に入って revision が進む
        doc.replace(0, 0, vec!["new0".to_string()], 1, "", "")
            .unwrap();
        assert_eq!(doc.line_count(), 5);

        // 古い base_rev を基準にした「line2（旧2行目）の置換」を適用
        doc.replace_with_base(
            base_rev,
            2,
            3,
            vec!["replaced_line2".to_string()],
            2,
            "",
            "",
        )
        .unwrap();

        // 現在座標（3行目）が置換されていること
        let lines = doc.read(0, doc.line_count()).unwrap();
        assert_eq!(
            lines,
            vec!["new0", "line0", "line1", "replaced_line2", "line3"]
        );
        std::fs::remove_file(path).ok();
    }


    #[test]
    fn bulk_replace_all_is_evaluated_on_demand_and_undoable() {
        let (mut doc, path) = disk_doc("bulk-test", &["foo 123", "bar 456", "foo 789"]);
        let base_rev = doc.revision();
        let pattern = regex::RegexBuilder::new("foo")
            .case_insensitive(true)
            .build()
            .unwrap();
        let op = crate::operation_log::BulkOperation::ReplaceAll {
            from_line: 0,
            to_line: 3,
            query: "foo".to_string(),
            replacement: "baz".to_string(),
            case_sensitive: false,
            pattern: std::sync::Arc::new(pattern),
        };
        doc.apply_bulk_operation(base_rev, 1, op, "", "").unwrap();

        // オンデマンドに評価されて置換結果が返ること
        let lines = doc.read(0, 3).unwrap();
        assert_eq!(lines, vec!["baz 123", "bar 456", "baz 789"]);

        // Undo すると瞬時に元に戻ること
        doc.undo().unwrap().unwrap();
        let restored = doc.read(0, 3).unwrap();
        assert_eq!(restored, vec!["foo 123", "bar 456", "foo 789"]);

        // Redo すると再度適用されること
        doc.redo().unwrap().unwrap();
        let reapplied = doc.read(0, 3).unwrap();
        assert_eq!(reapplied, vec!["baz 123", "bar 456", "baz 789"]);

        std::fs::remove_file(path).ok();
    }


    /// 回帰: 同一 group の連続編集で既存トランザクションの revision が上書きされず、
    /// 保存直後に続けてタイプしても saved checkpoint が壊れないことを検証する。
    #[test]
    fn merging_edits_keeps_revisions_immutable() {
        let (mut doc, path) = disk_doc("merge-revision", &["a", "b"]);
        // 保存して saved checkpoint を作る
        doc.save(&path).unwrap();
        let saved_rev = doc.revision();
        assert!(doc.is_clean());

        // 同一 group で連続タイプ（マージが発生する）
        doc.replace(0, 1, vec!["a1".into()], 1, "", "").unwrap();
        let rev1 = doc.revision();
        assert!(!doc.is_clean());
        doc.replace(0, 1, vec!["a12".into()], 1, "", "").unwrap();
        let rev2 = doc.revision();

        // マージは既存トランザクションへ追記するだけで、公開済みの revision を
        // 書き換えない（保存済み checkpoint が破壊されない）。
        assert_eq!(rev1, rev2, "マージで公開 revision を上書きしない");
        assert!(
            doc.log_validate_base_for_test(rev1).is_ok(),
            "公開済み revision は残る"
        );
        assert!(
            doc.log_validate_base_for_test(saved_rev).is_ok(),
            "saved checkpoint は残る"
        );
        assert!(!doc.is_clean(), "内容が変わっているので dirty のまま");

        // Undo すると saved 直後の状態へ 1 ステップで戻る（saved を飛び越さない）
        doc.undo().unwrap();
        assert!(doc.is_clean(), "Undo で saved checkpoint へ戻る");
        assert_eq!(all(&mut doc), vec!["a", "b"]);
        std::fs::remove_file(path).ok();
    }


    /// 回帰: Undo 後の Redo 枝にしか存在しない revision を基準にした編集を拒否する。
    #[test]
    fn validate_base_rejects_redo_branch_revisions() {
        let (mut doc, path) = disk_doc("redo-branch", &["x"]);
        doc.replace(0, 1, vec!["y".into()], 1, "", "").unwrap();
        let undone_rev = doc.revision();

        // Undo して現在 head を巻き戻す（undone_rev は Redo 枝にだけ残る）
        doc.undo().unwrap();
        assert_eq!(all(&mut doc), vec!["x"]);

        // Redo 枝の revision を base にした編集は拒否される
        let result = doc.replace_with_base(undone_rev, 0, 1, vec!["z".into()], 2, "", "");
        assert!(result.is_err(), "Redo 枝の revision は受け付けない");
        std::fs::remove_file(path).ok();
    }


    /// 回帰: 小さいファイルで bulk（すべて置換）を保存した後、
    /// 変換済みの行へ同じ規則が再適用されず（foo→foofoo の二重化）、
    /// Undo が正しく元の内容へ戻ることを検証する。
    #[test]
    fn bulk_replace_all_survives_save_without_double_apply() {
        let (mut doc, path) = disk_doc("bulk-save", &["foo a", "bar b", "foo c"]);
        let base_rev = doc.revision();
        let pattern = regex::RegexBuilder::new("foo").build().unwrap();
        let op = crate::operation_log::BulkOperation::ReplaceAll {
            from_line: 0,
            to_line: 3,
            query: "foo".to_string(),
            replacement: "baz".to_string(),
            case_sensitive: true,
            pattern: std::sync::Arc::new(pattern),
        };
        doc.apply_bulk_operation(base_rev, 1, op, "", "").unwrap();
        assert_eq!(all(&mut doc), vec!["baz a", "bar b", "baz c"]);

        // 保存（小さいファイルは Undo 履歴を保持する分岐）
        doc.save(&path).unwrap();
        assert!(doc.is_clean());

        // 二重適用されていないこと
        assert_eq!(all(&mut doc), vec!["baz a", "bar b", "baz c"]);

        // Undo で bulk が元へ戻ること（マテリアライズ済み splice として）
        doc.undo().unwrap();
        assert_eq!(all(&mut doc), vec!["foo a", "bar b", "foo c"]);

        // Redo で再適用されること
        doc.redo().unwrap();
        assert_eq!(all(&mut doc), vec!["baz a", "bar b", "baz c"]);
        std::fs::remove_file(path).ok();
    }


    /// 回帰: bulk 操作（すべて置換）の後に通常編集が入っても、
    /// 後続の新規テキストへ過去の bulk 規則が勝手に適用されず、
    /// Undo も各段階へ正しく戻ることを検証する。
    #[test]
    fn new_edits_after_bulk_are_not_transformed_retroactively() {
        let (mut doc, path) = disk_doc("bulk-edit-seq", &["foo 1", "bar 2", "foo 3"]);
        let base_rev = doc.revision();
        let pattern = regex::RegexBuilder::new("foo").build().unwrap();
        let op = crate::operation_log::BulkOperation::ReplaceAll {
            from_line: 0,
            to_line: 3,
            query: "foo".to_string(),
            replacement: "baz".to_string(),
            case_sensitive: true,
            pattern: std::sync::Arc::new(pattern),
        };
        doc.apply_bulk_operation(base_rev, 1, op, "", "").unwrap();
        assert_eq!(all(&mut doc), vec!["baz 1", "bar 2", "baz 3"]);

        // bulk 適用後に、範囲内に新たな "foo new" を通常挿入する
        doc.replace(1, 1, vec!["foo new".to_string()], 2, "", "foo new")
            .unwrap();

        // 挿入された "foo new" は "baz new" に書き換わらずそのまま残ること
        assert_eq!(all(&mut doc), vec!["baz 1", "foo new", "bar 2", "baz 3"]);

        // 1回目の Undo で通常編集（foo new の挿入）が取り消されること
        doc.undo().unwrap();
        assert_eq!(all(&mut doc), vec!["baz 1", "bar 2", "baz 3"]);

        // 2回目の Undo で bulk 操作自体が取り消されて初期状態に戻ること
        doc.undo().unwrap();
        assert_eq!(all(&mut doc), vec!["foo 1", "bar 2", "foo 3"]);

        std::fs::remove_file(path).ok();
    }

}
