use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::document::Document;
use crate::piece_tree::Piece;
use crate::source::{FileEncoding, ScanIndex, ScanState, Source, CHUNK, STRIDE};

impl Document {
    /// 文書の行を書き手へ流す。全文を 1 つの文字列に集めない。
    pub(crate) fn write_to<W: Write>(&mut self, out: &mut W) -> Result<(), String> {
        let mut broken = None;
        let encoding = self.encoding;
        let line_ending_bytes = encoding.encode_str(
            std::str::from_utf8(self.line_ending.as_bytes()).expect("line endings are ASCII"),
        );
        let bom: &[u8] = match encoding {
            FileEncoding::Utf8Bom => b"\xEF\xBB\xBF",
            FileEncoding::Utf16Le => b"\xFF\xFE",
            FileEncoding::Utf16Be => b"\xFE\xFF",
            _ => b"",
        };
        if let Err(e) = out.write_all(bom) {
            return Err(format!("書き込めませんでした: {e}"));
        }
        self.each_line(0, self.count, &mut |i, line| {
            let write = |out: &mut W| -> std::io::Result<()> {
                if i > 0 {
                    out.write_all(&line_ending_bytes)?;
                }
                let clean_line = line.trim_end_matches(['\r', '\n']);
                out.write_all(&encoding.encode_str(clean_line))
            };
            match write(out) {
                Ok(()) => true,
                Err(e) => {
                    broken = Some(format!("書き込めませんでした: {e}"));
                    false
                }
            }
        })?;
        match broken {
            Some(error) => Err(error),
            None => out
                .flush()
                .map_err(|e| format!("書き込めませんでした: {e}")),
        }
    }

    /// 文書をディスクへ流し、保存したファイルを新しい本体にする。
    /// 一時ファイルへ書いてから入れ替えるので、書きかけで元を壊さない。
    pub(crate) fn save(&mut self, path: &str) -> Result<(), String> {
        // bulk（すべて置換等）は遅延評価のため、保存で書き出す前に等価な splice
        // へ固める。これにより Undo の復元内容（元行）が読み取り時の bulk 再適用に
        // 汚染されず、下書きへも通常の差分として残せる。失敗したら保存自体を中止する。
        if self.log.has_active_bulk() {
            self.materialize_bulk_transactions()?;
        }
        let tmp = format!("{path}.saving");
        let fail = |e: String| format!("{path} を保存できませんでした: {e}");
        // 書きながら次の索引を作る。保存が終わった姿はこの索引そのもの。
        let bom: &[u8] = match self.encoding {
            FileEncoding::Utf8Bom => b"\xEF\xBB\xBF",
            FileEncoding::Utf16Le => b"\xFF\xFE",
            FileEncoding::Utf16Be => b"\xFE\xFF",
            _ => b"",
        };
        let initial_offset = bom.len() as u64;
        let mut marks = vec![initial_offset];
        let mut written = 0;
        let mut ends_with_newline = false;
        {
            let file = File::create(&tmp).map_err(|e| fail(e.to_string()))?;
            let mut out = BufWriter::with_capacity(CHUNK, file);
            out.write_all(bom).map_err(|e| fail(e.to_string()))?;
            written += initial_offset;
            let count = self.count;
            let mut broken = None;
            let encoding = self.encoding;
            let line_ending_bytes = encoding.encode_str(
                std::str::from_utf8(self.line_ending.as_bytes()).expect("line endings are ASCII"),
            );
            self.each_line(0, count, &mut |i, line| {
                let mut write = |out: &mut BufWriter<File>| -> std::io::Result<()> {
                    if i > 0 {
                        out.write_all(&line_ending_bytes)?;
                        written += line_ending_bytes.len() as u64;
                        if i % STRIDE == 0 {
                            marks.push(written);
                        }
                    }
                    let clean_line = line.trim_end_matches(['\r', '\n']);
                    ends_with_newline = i > 0 && clean_line.is_empty();
                    let encoded = encoding.encode_str(clean_line);
                    out.write_all(&encoded)?;
                    written += encoded.len() as u64;
                    Ok(())
                };
                match write(&mut out) {
                    Ok(()) => true,
                    Err(e) => {
                        broken = Some(e.to_string());
                        false
                    }
                }
            })?;
            if let Some(error) = broken {
                std::fs::remove_file(&tmp).ok();
                return Err(fail(error));
            }
            out.flush().map_err(|e| fail(e.to_string()))?;
        }
        // 自分が読んでいる元ファイルへ重ねる場合だけ、rename の直前に手を放す。
        // rename が失敗したら必ず戻す。Disk piece を残したまま Source だけ失うと、
        // その後の読みがパニックし、文書マップの Mutex まで poison される。
        let replacing_source = self
            .source
            .as_ref()
            .is_some_and(|source| source.path == Path::new(path));
        let old_source = replacing_source.then(|| self.source.take()).flatten();
        if let Err(error) = std::fs::rename(&tmp, path) {
            self.source = old_source;
            std::fs::remove_file(&tmp).ok();
            return Err(fail(error.to_string()));
        }
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) => {
                self.source = old_source;
                return Err(fail(error.to_string()));
            }
        };
        let modified = file.metadata().ok().and_then(|meta| meta.modified().ok());
        self.source = Some(Source {
            path: Path::new(path).to_path_buf(),
            file,
            index: Arc::new(ScanIndex {
                state: Mutex::new(ScanState {
                    marks,
                    lines: self.count,
                    done: true,
                    broken: None,
                }),
            }),
            bytes: written,
            content_offset: initial_offset,
            pending_from: None,
            ends_with_newline,
            modified,
            encoding: self.encoding,
            line_ending: self.line_ending,
        });
        self.pieces = crate::piece_tree::PieceTree::new(vec![Piece::Source {
            from: initial_offset as usize,
            len: written as usize - initial_offset as usize,
            newlines: self.count.saturating_sub(1),
            starts_newline: false,
            ends_newline: false,
        }]);
        // 小さいファイルは保存後も Undo 履歴を保持し、
        // 巨大ファイル（10MB超）は新しいベースへ切り替えて操作ログと編集実体を破棄しメモリを解放する。
        if written > 10 * 1024 * 1024 {
            self.log.clear();
            self.buffers = crate::edit_buffers::EditBuffers::default();
        } else {
            self.materialize_bulk_transactions()?;
            self.log.mark_saved();
        }
        // 保存で新しいバイト列へ差し替わったため、旧バイト列の索引は破棄する。
        // 共有状態を持つバックグラウンド索引は、Arc の所有者がここだけになると自発的に終わる。
        self.search_index = None;
        Ok(())
    }

    /// 保存で bulk の結果が新しい Source へ書き込まれたため、active な bulk
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

    /// 下書きを書き出す。元ファイルがある場合は全文ダンプを絶対に行わず、
    /// 元ファイルへの参照、総行数、未保存の差分行、および Redo 履歴を含む head 位置を記録する。
    pub(crate) fn write_draft<W: Write>(
        &mut self,
        out: &mut W,
        path: Option<&str>,
    ) -> Result<(), String> {
        if self.log.has_active_bulk() {
            self.materialize_bulk_transactions()?;
        }
        if let Some(p) = path {
            writeln!(out, "// PLANETEXT_DRAFT_REF_V2")
                .map_err(|e| format!("下書きを保存できませんでした: {e}"))?;
            writeln!(out, "{p}").map_err(|e| format!("下書きを保存できませんでした: {e}"))?;
            let (diffs, head_offset) = self.collect_draft_diffs();
            let active_count = head_offset.min(diffs.len());
            let mut line_delta: isize = 0;
            for diff in &diffs[..active_count] {
                line_delta += diff.lines.len() as isize - diff.removed_lines as isize;
            }
            let base_count = (self.count as isize - line_delta).max(0) as usize;
            writeln!(out, "{}", base_count)
                .map_err(|e| format!("下書きを保存できませんでした: {e}"))?;
            if diffs.is_empty() {
                writeln!(out, "CLEAN").map_err(|e| format!("下書きを保存できませんでした: {e}"))?;
            } else {
                writeln!(out, "DIFF").map_err(|e| format!("下書きを保存できませんでした: {e}"))?;
                writeln!(out, "{} {}", diffs.len(), head_offset)
                    .map_err(|e| format!("下書きを保存できませんでした: {e}"))?;
                for diff in &diffs {
                    diff.write_to(out)
                        .map_err(|e| format!("下書きを保存できませんでした: {e}"))?;
                }
            }
            Ok(())
        } else {
            // 無題ドキュメントは1行目を空にして本文を書き出す
            writeln!(out).map_err(|e| format!("下書きを保存できませんでした: {e}"))?;
            self.write_to(out)
        }
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

    /// 元ファイルから開いた文書に対して、未保存の操作ログを時系列順に再生し、
    /// メモリ上のピースツリーを編集状態へ再構成する。
    /// 下書きに記録された deleted_lines を使用するため、元ファイルへのディスクリードは一切発生しない。
    pub(crate) fn apply_draft_diffs(&mut self, diffs: Vec<DraftDiff>) -> Result<(), String> {
        for diff in diffs {
            let to_line = diff.from_line + diff.removed_lines;
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
        Ok(())
    }
}

/// 下書きに記録される行範囲の変更差分。外部クレートに頼らない軽量なプレーンテキスト表現。
#[derive(Clone, Debug)]
pub struct DraftDiff {
    pub group: u64,
    pub from_line: usize,
    pub removed_lines: usize,
    pub lines: Vec<String>,
    pub deleted_lines: Vec<String>,
    pub before: String,
    pub after: String,
}

impl DraftDiff {
    pub(crate) fn write_to<W: Write>(&self, out: &mut W) -> std::io::Result<()> {
        writeln!(
            out,
            "{} {} {} {} {}",
            self.group,
            self.from_line,
            self.removed_lines,
            self.lines.len(),
            self.deleted_lines.len()
        )?;
        writeln!(
            out,
            "{}",
            if self.before.is_empty() {
                "-"
            } else {
                &self.before
            }
        )?;
        writeln!(
            out,
            "{}",
            if self.after.is_empty() {
                "-"
            } else {
                &self.after
            }
        )?;
        for line in &self.lines {
            writeln!(out, "{line}")?;
        }
        for line in &self.deleted_lines {
            writeln!(out, "{line}")?;
        }
        Ok(())
    }

    pub(crate) fn read_from<'a>(lines: &mut impl Iterator<Item = &'a str>) -> Option<Self> {
        let header = lines.next()?;
        let parts: Vec<&str> = header.split_whitespace().collect();
        let (group, from_line, removed_lines, count, deleted_count) = if parts.len() >= 5 {
            (
                parts[0].parse().ok()?,
                parts[1].parse().ok()?,
                parts[2].parse().ok()?,
                parts[3].parse::<usize>().ok()?,
                parts[4].parse::<usize>().ok()?,
            )
        } else if parts.len() >= 4 {
            (
                parts[0].parse().ok()?,
                parts[1].parse().ok()?,
                parts[2].parse().ok()?,
                parts[3].parse::<usize>().ok()?,
                0,
            )
        } else if parts.len() == 3 {
            (
                0,
                parts[0].parse().ok()?,
                parts[1].parse().ok()?,
                parts[2].parse::<usize>().ok()?,
                0,
            )
        } else {
            return None;
        };
        let before_line = lines.next()?;
        let after_line = lines.next()?;
        let before = if before_line == "-" {
            String::new()
        } else {
            before_line.to_string()
        };
        let after = if after_line == "-" {
            String::new()
        } else {
            after_line.to_string()
        };
        let mut diff_lines = Vec::with_capacity(count);
        for _ in 0..count {
            diff_lines.push(lines.next()?.to_string());
        }
        let mut deleted_lines = Vec::with_capacity(deleted_count);
        for _ in 0..deleted_count {
            deleted_lines.push(lines.next()?.to_string());
        }
        Some(Self {
            group,
            from_line,
            removed_lines,
            lines: diff_lines,
            deleted_lines,
            before,
            after,
        })
    }

    /// 旧フォーマット（V1: before/after/deleted_lines なし）からの読み出し
    pub(crate) fn read_from_v1<'a>(lines: &mut impl Iterator<Item = &'a str>) -> Option<Self> {
        let header = lines.next()?;
        let parts: Vec<&str> = header.split_whitespace().collect();
        let (group, from_line, removed_lines, count) = if parts.len() >= 4 {
            (
                parts[0].parse().ok()?,
                parts[1].parse().ok()?,
                parts[2].parse().ok()?,
                parts[3].parse::<usize>().ok()?,
            )
        } else if parts.len() == 3 {
            (
                0,
                parts[0].parse().ok()?,
                parts[1].parse().ok()?,
                parts[2].parse::<usize>().ok()?,
            )
        } else {
            return None;
        };
        let mut diff_lines = Vec::with_capacity(count);
        for _ in 0..count {
            diff_lines.push(lines.next()?.to_string());
        }
        let fallback_pos = format!("{from_line}.0-{from_line}.0");
        Some(Self {
            group,
            from_line,
            removed_lines,
            lines: diff_lines,
            deleted_lines: Vec::new(),
            before: fallback_pos.clone(),
            after: fallback_pos,
        })
    }
}
