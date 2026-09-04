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
use crate::search::ScanHit;
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
    pub(crate) pieces: PieceTree,
    pub(crate) buffers: EditBuffers,
    /// すべてのピースの行数の合計。
    pub(crate) count: usize,
    pub(crate) log: OperationLog,
    pub(crate) encoding: FileEncoding,
    pub(crate) line_ending: LineEnding,
    pub(crate) pending_source: Option<PendingSource>,
    pub(crate) search_index: Option<SearchIndex>,
    pub(crate) background_index: Option<BackgroundIndex>,
    pub(crate) pending_redo_diffs: Vec<DraftDiff>,
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
        if scan_done {
            self.confirm_scan();
            return self.read(self.count.saturating_sub(count), count);
        }
        self.source
            .as_mut()
            .ok_or_else(|| "末尾を読むファイルがありません".to_string())?
            .read_tail(count)
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
