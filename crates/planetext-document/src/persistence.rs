use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Arc;

use crate::document::Document;
use crate::source::{FileEncoding, ScanIndex, ScanState, Source, CHUNK, STRIDE};

impl Document {
    /// 文書の行を書き手へ流す。全文を 1 つの文字列に集めない。
    pub(crate) fn write_to<W: Write>(&mut self, out: &mut W) -> Result<(), String> {
        let mut broken = None;
        let encoding = self.encoding();
        let line_ending = self.line_ending();
        let line_ending_bytes = encoding.encode_str(
            std::str::from_utf8(line_ending.as_bytes()).expect("line endings are ASCII"),
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
        self.each_line(0, self.line_count(), &mut |i, line| {
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
        if self.has_active_bulk() {
            self.materialize_bulk_transactions()?;
        }
        let tmp = format!("{path}.saving");
        let fail = |e: String| format!("{path} を保存できませんでした: {e}");
        let encoding = self.encoding();
        let line_ending = self.line_ending();
        // 書きながら次の索引を作る。保存が終わった姿はこの索引そのもの。
        let bom: &[u8] = match encoding {
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
            let count = self.line_count();
            let mut broken = None;
            let line_ending_bytes = encoding.encode_str(
                std::str::from_utf8(line_ending.as_bytes()).expect("line endings are ASCII"),
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
        let replacing_source = self.is_same_source_path(path);
        let old_source = replacing_source.then(|| self.take_source()).flatten();
        if let Err(error) = std::fs::rename(&tmp, path) {
            self.restore_source(old_source);
            std::fs::remove_file(&tmp).ok();
            return Err(fail(error.to_string()));
        }
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) => {
                self.restore_source(old_source);
                return Err(fail(error.to_string()));
            }
        };
        let modified = file.metadata().ok().and_then(|meta| meta.modified().ok());
        let count = self.line_count();
        let new_source = Source {
            path: Path::new(path).to_path_buf(),
            file,
            index: Arc::new(ScanIndex::new(ScanState {
                marks,
                lines: count,
                done: true,
                broken: None,
            })),
            bytes: written,
            content_offset: initial_offset,
            pending_from: None,
            ends_with_newline,
            modified,
            encoding,
            line_ending,
        };
        let is_small_file = written <= 10 * 1024 * 1024;
        self.reinitialize_after_save(new_source, is_small_file);
        if is_small_file {
            self.materialize_bulk_transactions()?;
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
        if self.has_active_bulk() {
            self.materialize_bulk_transactions()?;
        }
        if let Some(p) = path {
            writeln!(out, "// PLANETEXT_DRAFT_REF_V2")
                .map_err(|e| format!("下書きを保存できませんでした: {e}"))?;
            writeln!(out, "{p}").map_err(|e| format!("下書きを保存できませんでした: {e}"))?;
            let (diffs, head_offset) = self.collect_draft_diffs();
            // 過去の重大ミスと再発防止の記録:
            // 800MB 等の巨大ファイル読み込み時、背景走査が完了していない段階で下書きが保存されると、
            // self.count は「先頭 1MB の読み込みバッファ内の改行数（20,971）」という仮の数値にすぎない。
            // それを base_count として下書きに保存してしまうと、次回起動時にその仮の数値を「確定した真の行数」
            // と誤認して 800MB のファイルの走査を 20,971 行で打ち切る致命的な先祖返り事故を引き起こした。
            // したがって、走査未完了（confirmed_line_count() が None）のときは、仮の行数は絶対に下書きに書かず、
            // 未確定を示す 0 を書き出さなければならない。この不変条件を絶対に崩してはならない。
            let base_count = if let Some(confirmed) = self.confirmed_line_count() {
                let active_count = head_offset.min(diffs.len());
                let mut line_delta: isize = 0;
                for diff in &diffs[..active_count] {
                    line_delta += diff.lines.len() as isize - diff.removed_lines as isize;
                }
                (confirmed as isize - line_delta).max(0) as usize
            } else {
                0
            };
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
