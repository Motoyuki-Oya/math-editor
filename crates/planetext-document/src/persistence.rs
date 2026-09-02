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
            self.log.mark_saved();
        }
        Ok(())
    }

    /// 下書きを書き出す。元ファイルがある場合は全文ダンプを絶対に行わず、
    /// 元ファイルへの参照と未保存の差分行（JSON）だけを記録する。
    pub(crate) fn write_draft<W: Write>(
        &mut self,
        out: &mut W,
        path: Option<&str>,
    ) -> Result<(), String> {
        if let Some(p) = path {
            writeln!(out, "// PLANETEXT_DRAFT_REF_V1")
                .map_err(|e| format!("下書きを保存できませんでした: {e}"))?;
            writeln!(out, "{p}").map_err(|e| format!("下書きを保存できませんでした: {e}"))?;
            if self.is_clean() {
                writeln!(out, "CLEAN").map_err(|e| format!("下書きを保存できませんでした: {e}"))?;
            } else {
                writeln!(out, "DIFF").map_err(|e| format!("下書きを保存できませんでした: {e}"))?;
                let diffs = self.collect_draft_diffs();
                writeln!(out, "{}", diffs.len())
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

    /// 未保存のトランザクションから、変更された行範囲の差分データを抽出する。
    pub(crate) fn collect_draft_diffs(&mut self) -> Vec<DraftDiff> {
        let edits: Vec<(usize, usize, usize)> = self
            .log
            .unsaved_transactions()
            .iter()
            .flat_map(|tx| {
                tx.edits()
                    .iter()
                    .map(|e| (e.from_line, e.removed_lines, e.inserted_lines))
            })
            .collect();
        let mut diffs = Vec::new();
        for (from_line, removed_lines, inserted_lines) in edits {
            let mut lines = Vec::new();
            let _ = self.each_line(from_line, inserted_lines, &mut |_, line| {
                lines.push(line.to_string());
                true
            });
            diffs.push(DraftDiff {
                from_line,
                removed_lines,
                lines,
            });
        }
        diffs
    }

    /// 元ファイルから開いた文書に対して、下書き差分を適用して編集状態を復元する。
    pub(crate) fn apply_draft_diffs(&mut self, diffs: Vec<DraftDiff>) -> Result<(), String> {
        for diff in diffs {
            let to_line = diff.from_line + diff.removed_lines;
            self.replace(diff.from_line, to_line, diff.lines, 0, "", "")?;
        }
        self.log.mark_dirty_without_history();
        Ok(())
    }
}

/// 下書きに記録される行範囲の変更差分。外部クレートに頼らない軽量なプレーンテキスト表現。
#[derive(Clone, Debug)]
pub struct DraftDiff {
    pub from_line: usize,
    pub removed_lines: usize,
    pub lines: Vec<String>,
}

impl DraftDiff {
    pub(crate) fn write_to<W: Write>(&self, out: &mut W) -> std::io::Result<()> {
        writeln!(
            out,
            "{} {} {}",
            self.from_line,
            self.removed_lines,
            self.lines.len()
        )?;
        for line in &self.lines {
            writeln!(out, "{line}")?;
        }
        Ok(())
    }

    pub(crate) fn read_from<'a>(lines: &mut impl Iterator<Item = &'a str>) -> Option<Self> {
        let header = lines.next()?;
        let mut parts = header.split_whitespace();
        let from_line = parts.next()?.parse().ok()?;
        let removed_lines = parts.next()?.parse().ok()?;
        let count: usize = parts.next()?.parse().ok()?;
        let mut diff_lines = Vec::with_capacity(count);
        for _ in 0..count {
            diff_lines.push(lines.next()?.to_string());
        }
        Some(Self {
            from_line,
            removed_lines,
            lines: diff_lines,
        })
    }
}
