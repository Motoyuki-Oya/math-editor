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
use crate::operation_log::{Edit, OperationLog, Step, HISTORY_LIMIT};
use crate::piece_tree::{Piece, PieceTree};
use crate::source::{BackgroundScan, FileEncoding, LineEnding, ScanIndex, Source};

pub(crate) struct Document {
    pub(crate) source: Option<Source>,
    pub(crate) pieces: PieceTree,
    pub(crate) buffers: EditBuffers,
    /// すべてのピースの行数の合計。
    pub(crate) count: usize,
    pub(crate) log: OperationLog,
    pub(crate) encoding: FileEncoding,
    pub(crate) line_ending: LineEnding,
    pending_source_newlines: Option<usize>,
}

/// 元に戻す・やり直すの結果: 復元すべき控えと、行が変わった範囲の始まり。
/// frontend は `touched_from` から先の手元の行を捨てて取り寄せ直す。
pub(crate) struct Restored {
    pub(crate) state: String,
    pub(crate) touched_from: usize,
    pub(crate) line_count: usize,
}

impl Document {
    pub(crate) fn open(path: &str) -> Result<(Document, Option<BackgroundScan>), String> {
        Self::open_with_encoding(path, None)
    }

    pub(crate) fn open_with_encoding(
        path: &str,
        encoding: Option<FileEncoding>,
    ) -> Result<(Document, Option<BackgroundScan>), String> {
        let (source, scan) = Source::open_with_encoding(Path::new(path), encoding)?;
        let count = source.lines();
        let provisional_newlines = count.saturating_sub(1);
        let pending_source_newlines = scan.as_ref().map(|_| provisional_newlines);
        let enc = source.encoding;
        let line_ending = source.line_ending;
        let content_offset = source.content_offset as usize;
        Ok((
            Document {
                pieces: PieceTree::new(vec![Piece::Source {
                    from: content_offset,
                    len: source.bytes as usize - content_offset,
                    newlines: provisional_newlines,
                    starts_newline: false,
                    ends_newline: false,
                }]),
                buffers: EditBuffers::default(),
                count,
                encoding: enc,
                line_ending,
                source: Some(source),
                log: OperationLog::default(),
                pending_source_newlines,
            },
            scan,
        ))
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
            pending_source_newlines: None,
        }
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
        self.pending_source_newlines = scan.as_ref().map(|_| count.saturating_sub(1));
        self.encoding = encoding;
        self.line_ending = new_source.line_ending;
        let content_offset = new_source.content_offset as usize;
        self.pieces = PieceTree::new(vec![Piece::Source {
            from: content_offset,
            len: new_source.bytes as usize - content_offset,
            newlines: count.saturating_sub(1),
            starts_newline: false,
            ends_newline: false,
        }]);
        self.buffers = EditBuffers::default();
        self.log.clear();
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

    /// 検索スレッドへ渡す読み取り専用の姿。ファイルカーソルは独立し、編集の
    /// ピースは開始時点の内容を複製するので、文書ロックを持たずに走査できる。
    pub(crate) fn search_snapshot(&self) -> Result<Document, String> {
        Ok(Document {
            source: self.source.as_ref().map(Source::search_copy).transpose()?,
            pieces: PieceTree::new(self.pieces.pieces()),
            buffers: self.buffers.clone(),
            count: self.count,
            log: OperationLog::default(),
            encoding: self.encoding,
            line_ending: self.line_ending,
            pending_source_newlines: self.pending_source_newlines,
        })
    }

    /// 走査完了後に呼ぶ。未走査だった元ファイル末尾の集約値だけを確定する。
    pub(crate) fn confirm_scan(&mut self) {
        let Some(provisional_newlines) = self.pending_source_newlines.take() else {
            return;
        };
        let Some(source) = self.source.as_ref() else {
            return;
        };
        let exact_newlines = source.lines().saturating_sub(1);
        self.pieces
            .add_newlines_to_last_source(exact_newlines.saturating_sub(provisional_newlines));
        self.count = self.pieces.line_count();
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
                        &mut |i, text| f(start + skip + i, text),
                    )
                }
                Piece::Source { from, len, .. } => match self.source.as_mut() {
                    Some(source) => {
                        match source.for_each_range_line(from, len, skip, take, &mut |i, text| {
                            f(start + skip + i, text)
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

    /// `from..to` の行を `lines` に置き換え、逆操作を履歴に書く。
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
        if from > to || to > self.count {
            return Err("置き換えの範囲が文書の外です".to_string());
        }
        let clean_lines = lines
            .into_iter()
            .map(|line| line.trim_end_matches(['\r', '\n']).to_string())
            .collect();
        let edit = self.splice(from, to, clean_lines)?;
        self.log.redo.clear();
        match self.log.undo.last_mut() {
            Some(step) if step.group == group => {
                step.edits.push(edit);
                step.after = after.to_string();
            }
            _ => {
                self.log.undo.push(Step {
                    group,
                    edits: vec![edit],
                    before: before.to_string(),
                    after: after.to_string(),
                });
                if self.log.undo.len() > HISTORY_LIMIT {
                    self.log.undo.remove(0);
                }
            }
        }
        Ok(self.count)
    }

    /// 置き換えの本体。履歴には触らず、逆操作を返す。取り除く行の中身は
    /// ここでディスクから控える（元に戻すために要る）。
    fn splice(&mut self, from: usize, to: usize, lines: Vec<String>) -> Result<Edit, String> {
        let removed_count = to - from;
        let removed = self.read(from, removed_count)?;
        if removed.len() != removed_count {
            return Err("置き換える範囲が大きすぎます".to_string());
        }
        let old_count = self.count;
        let a = self.split(from)?;
        let b = self.split(to)?;
        let inserted = lines.len();
        let starts_newline = from == to && from == old_count && from > 0;
        let ends_newline = to < old_count;
        let fresh = (!lines.is_empty()).then(|| {
            let (range, newlines) = self.buffers.append_lines_with_boundaries(
                &lines,
                self.encoding,
                self.line_ending,
                starts_newline,
                ends_newline,
            );
            Piece::Edit {
                from: range.from,
                len: range.len,
                newlines,
                starts_newline,
                ends_newline,
                encoding: range.encoding,
                line_ending: range.line_ending,
            }
        });
        self.pieces.replace(a, b, fresh.into_iter().collect());
        if inserted == 0 && to == old_count && from > 0 {
            let separator_len = self.source.as_ref().map_or_else(
                || EditBuffers::line_separator_len(self.encoding, self.line_ending),
                |source| EditBuffers::line_separator_len(source.encoding, source.line_ending),
            );
            self.pieces.trim_trailing_newline(separator_len);
        }
        self.count = old_count - removed_count + inserted;
        if self.count == 0 {
            // 文書は少なくとも 1 行。frontend のモデルも空文書を 1 行と数える。
            let (range, _) =
                self.buffers
                    .append_lines(&[String::new()], self.encoding, self.line_ending);
            self.pieces.insert(
                0,
                Piece::Edit {
                    from: range.from,
                    len: range.len,
                    newlines: 0,
                    starts_newline: false,
                    ends_newline: false,
                    encoding: range.encoding,
                    line_ending: range.line_ending,
                },
            );
            self.count = 1;
        }
        debug_assert_eq!(self.count, self.pieces.line_count());
        debug_assert_eq!(self.count.saturating_sub(1), self.pieces.newline_count);
        let removed = self
            .log
            .append_deleted(&removed, self.encoding, self.line_ending);
        Ok(Edit {
            from,
            removed,
            inserted,
        })
    }

    /// ピース列を行 `line` の前で切り、その位置のピース番号を返す。
    fn split(&mut self, line: usize) -> Result<usize, String> {
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
                Piece::Source { from, len, .. } => {
                    let source = self.source.as_mut().ok_or_else(|| {
                        "文書ストアのディスク参照が失われました。開き直してください".to_string()
                    })?;
                    source.byte_offset_after_lines(*from, *len, skip)?
                }
            };
            let newlines = skip + leading;
            self.pieces.split_piece(index, byte, newlines, byte > 0);
            return Ok(index + 1);
        }
        Ok(index)
    }

    pub(crate) fn undo(&mut self) -> Result<Option<Restored>, String> {
        let Some(step) = self.log.undo.pop() else {
            return Ok(None);
        };
        let (reverted, touched_from) = self.revert(&step)?;
        let state = step.before.clone();
        self.log.redo.push(reverted);
        Ok(Some(Restored {
            state,
            touched_from,
            line_count: self.count,
        }))
    }

    pub(crate) fn redo(&mut self) -> Result<Option<Restored>, String> {
        let Some(step) = self.log.redo.pop() else {
            return Ok(None);
        };
        let (reverted, touched_from) = self.revert(&step)?;
        let state = step.after.clone();
        self.log.undo.push(reverted);
        Ok(Some(Restored {
            state,
            touched_from,
            line_count: self.count,
        }))
    }

    /// ステップの置き換えを新しい順に巻き戻し、巻き戻し自体を巻き戻すステップを返す。
    fn revert(&mut self, step: &Step) -> Result<(Step, usize), String> {
        let mut inverse = Vec::with_capacity(step.edits.len());
        let mut touched_from = usize::MAX;
        for edit in step.edits.iter().rev() {
            touched_from = touched_from.min(edit.from);
            inverse.push(self.splice(
                edit.from,
                edit.from + edit.inserted,
                self.log.read_deleted(edit.removed),
            )?);
        }
        // 巻き戻しの逆は元のステップと同じ向き。適用した順の逆で持つ。
        Ok((
            Step {
                group: step.group,
                edits: inverse.into_iter().rev().collect(),
                before: step.before.clone(),
                after: step.after.clone(),
            },
            if touched_from == usize::MAX {
                0
            } else {
                touched_from
            },
        ))
    }

    /// 選択された範囲をひとつなぎのテキストにする。`first` / `last` は端の行の
    /// 切り出し（`None` なら行を丸ごと）。`overrides` の行は差し替えて使う。
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
            true
        })?;
        Ok(out)
    }
}
