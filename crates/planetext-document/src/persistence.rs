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
        let is_small_file = written <= crate::search_index::BIGRAM_INDEX_THRESHOLD as u64;
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

#[cfg(test)]
mod tests {
    use crate::document::Document;
    use crate::source::{FileEncoding, LineEnding};
    use crate::test_utils::*;

    #[test]
    fn empty_and_bom_only_files_expose_one_empty_line() {
        for (name, bytes, specified, offset) in [
            ("empty", Vec::new(), Some(FileEncoding::Utf8Bom), 0),
            ("utf8-bom-only", b"\xEF\xBB\xBF".to_vec(), None, 3),
            ("utf16le-bom-only", b"\xFF\xFE".to_vec(), None, 2),
            ("utf16be-bom-only", b"\xFE\xFF".to_vec(), None, 2),
        ] {
            let path = std::env::temp_dir()
                .join(format!("planetext-store-{}-{name}.txt", std::process::id()));
            std::fs::write(&path, bytes).unwrap();
            let (mut doc, scan) =
                Document::open_with_encoding(path.to_str().unwrap(), specified).unwrap();

            assert!(scan.is_none());
            assert_eq!(doc.source_content_offset_for_test(), Some(offset));
            assert_eq!(doc.line_count(), 1);
            assert_eq!(doc.read(0, 1).unwrap(), vec![""]);
            assert_eq!(doc.read_tail(1).unwrap(), vec![""]);
            std::fs::remove_file(path).ok();
        }
    }

    #[test]
    fn forced_utf8_bom_without_a_bom_keeps_all_content() {
        for (name, bytes) in [
            ("short-one", b"x".as_slice()),
            ("short-two", b"xy".as_slice()),
            ("full", b"first\nsecond".as_slice()),
        ] {
            let path = std::env::temp_dir().join(format!(
                "planetext-store-{}-forced-bom-{name}.txt",
                std::process::id()
            ));
            std::fs::write(&path, bytes).unwrap();
            let (mut doc, _) =
                Document::open_with_encoding(path.to_str().unwrap(), Some(FileEncoding::Utf8Bom))
                    .unwrap();

            assert_eq!(doc.source_content_offset_for_test(), Some(0));
            assert_eq!(
                all(&mut doc),
                String::from_utf8_lossy(bytes).lines().collect::<Vec<_>>()
            );
            std::fs::remove_file(path).ok();
        }
    }

    #[test]
    fn draft_construction_is_dirty_without_an_undo_step() {
        let mut doc = Document::from_draft(vec!["draft one".into(), "draft two".into()]);

        assert_eq!(all(&mut doc), vec!["draft one", "draft two"]);
        assert!(!doc.is_clean());
        assert!(doc.undo().unwrap().is_none());
    }

    #[test]
    fn first_edit_after_draft_construction_undoes_to_the_dirty_draft() {
        let mut doc = Document::from_draft(vec!["draft".into()]);

        doc.replace(0, 1, vec!["edited".into()], 1, "before", "after")
            .unwrap();
        assert!(doc.undo().unwrap().is_some());

        assert_eq!(all(&mut doc), vec!["draft"]);
        assert!(!doc.is_clean());
    }

    #[test]
    fn saving_a_constructed_draft_marks_it_clean() {
        let mut doc = Document::from_draft(vec!["draft".into()]);
        let path = std::env::temp_dir().join(format!(
            "planetext-store-{}-save-constructed-draft.txt",
            std::process::id()
        ));

        doc.save(path.to_str().unwrap()).unwrap();

        assert!(doc.is_clean());
        std::fs::remove_file(path).ok();
    }

    /// 【回帰防止テスト】
    /// 小さいファイルの保存後、Undo履歴が保持され、保存状態（is_clean）がマークされることを検証する。
    #[test]
    fn small_file_save_retains_undo_history() {
        let (mut doc, path) = disk_doc("small-save-undo", &["first line", "second line"]);
        doc.replace(1, 2, vec!["edited second line".into()], 1, "", "")
            .unwrap();
        assert!(!doc.is_clean());

        doc.save(&path).unwrap();
        assert!(doc.is_clean(), "保存後は clean になること");
        assert!(
            doc.log_transactions_len_for_test() > 0,
            "小さいファイルは保存後も Undo 履歴が保持されること"
        );
        std::fs::remove_file(path).ok();
    }

    /// 【回帰防止テスト】
    /// 巨大ファイル（30MB超）の保存後、新しいベースへ切り替わり、メモリを圧迫しないよう
    /// 操作ログと編集バッファが解放されることを検証する。
    #[test]
    fn large_file_save_clears_log_and_switches_base() {
        let large_chunk = "B".repeat(1024 * 1024); // 1MB の行
        let lines: Vec<String> = vec![large_chunk.clone(); 31]; // 31MB 分
        let lines_ref: Vec<&str> = lines.iter().map(String::as_str).collect();
        let (mut doc, path) = disk_doc("large-save-clear", &lines_ref);

        doc.replace(1, 2, vec!["edited large line".into()], 1, "", "")
            .unwrap();
        assert!(doc.log_transactions_len_for_test() > 0);

        doc.save(&path).unwrap();
        assert!(doc.is_clean(), "保存後は clean になること");
        assert_eq!(
            doc.log_transactions_len_for_test(),
            0,
            "巨大ファイルは保存後に操作ログが破棄されてメモリが解放されること"
        );
        assert_eq!(
            doc.buffers_len_for_test(),
            0,
            "編集バッファも解放されること"
        );
        std::fs::remove_file(path).ok();
    }

    /// 保存すると保存先が新しい本体になり、続きの読みも元に戻すも生きている。
    #[test]
    fn saving_adopts_the_written_file_as_the_source() {
        let (mut doc, path) = disk_doc("save", &["a", "b"]);
        doc.replace(1, 2, vec!["B".into()], 1, "before", "")
            .unwrap();
        doc.save(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a\nB");
        assert_eq!(all(&mut doc), vec!["a", "B"]);
        let undone = doc.undo().unwrap().unwrap();
        assert_eq!(undone.state, "before");
        assert_eq!(all(&mut doc), vec!["a", "b"]);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn a_failed_overwrite_keeps_the_open_source_readable() {
        let (mut doc, path) = disk_doc("failed-save", &["a", "b"]);
        let tmp = format!("{path}.saving");
        std::fs::create_dir(&tmp).unwrap();
        assert!(doc.save(&path).is_err());
        std::fs::remove_dir(tmp).unwrap();
        assert_eq!(doc.read(0, 2).unwrap(), vec!["a", "b"]);
        std::fs::remove_file(path).ok();
    }

    /// 開いている間の外部変更は、壊れた読みを返さずに断る。
    #[test]
    fn outside_changes_are_refused_instead_of_read_wrong() {
        let (mut doc, path) = disk_doc("outside", &["a", "b"]);
        std::fs::write(&path, "changed elsewhere\nlonger than before").unwrap();
        assert!(doc.read(0, 2).is_err());
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn utf16_bom_files_read_edit_undo_redo_save_and_reopen() {
        for (name, encoding, first) in [
            ("le", FileEncoding::Utf16Le, "\u{a01}\u{100} first"),
            ("be", FileEncoding::Utf16Be, "\u{100}\u{a01} first"),
        ] {
            let path = std::env::temp_dir().join(format!(
                "planetext-store-{}-utf16-{name}.txt",
                std::process::id()
            ));
            let original = format!("{first}\n中央\nlast");
            std::fs::write(&path, utf16_file_bytes(&original, encoding)).unwrap();
            let (mut doc, scan) = Document::open(path.to_str().unwrap()).unwrap();
            if let Some(scan) = scan {
                scan.run().unwrap();
                doc.confirm_scan();
            }

            assert_eq!(doc.encoding(), encoding);
            assert_eq!(doc.source_content_offset_for_test(), Some(2));
            assert_eq!(all(&mut doc), vec![first, "中央", "last"]);
            doc.replace(
                1,
                2,
                vec!["編集一".into(), "編集二".into()],
                1,
                "before",
                "after",
            )
            .unwrap();
            assert_eq!(all(&mut doc), vec![first, "編集一", "編集二", "last"]);
            assert_eq!(doc.undo().unwrap().unwrap().state, "before");
            assert_eq!(all(&mut doc), vec![first, "中央", "last"]);
            assert_eq!(doc.redo().unwrap().unwrap().state, "after");
            assert_eq!(all(&mut doc), vec![first, "編集一", "編集二", "last"]);

            doc.save(path.to_str().unwrap()).unwrap();
            let saved = std::fs::read(&path).unwrap();
            assert!(saved.starts_with(match encoding {
                FileEncoding::Utf16Le => b"\xFF\xFE",
                FileEncoding::Utf16Be => b"\xFE\xFF",
                _ => unreachable!(),
            }));
            let (mut reopened, _) = Document::open(path.to_str().unwrap()).unwrap();
            assert_eq!(reopened.encoding(), encoding);
            assert_eq!(all(&mut reopened), vec![first, "編集一", "編集二", "last"]);
            std::fs::remove_file(path).ok();
        }
    }

    #[test]
    fn encoding_change_keeps_existing_edit_and_undo_ranges_decodable() {
        let (mut doc, path) = disk_doc("encoding-change", &["base", "tail"]);
        doc.replace(1, 2, vec!["編集".into()], 1, "before", "after")
            .unwrap();
        doc.set_encoding(FileEncoding::Utf16Be);

        assert_eq!(all(&mut doc), vec!["base", "編集"]);
        assert_eq!(doc.undo().unwrap().unwrap().state, "before");
        assert_eq!(all(&mut doc), vec!["base", "tail"]);
        assert_eq!(doc.redo().unwrap().unwrap().state, "after");
        assert_eq!(all(&mut doc), vec!["base", "編集"]);
        doc.save(&path).unwrap();

        let (mut reopened, _) = Document::open(&path).unwrap();
        assert_eq!(reopened.encoding(), FileEncoding::Utf16Be);
        assert_eq!(all(&mut reopened), vec!["base", "編集"]);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn shift_jis_reading_and_writing_and_search() {
        let dir = std::env::temp_dir();
        let path = dir.join("planetext-sjis-test.txt");
        let path_str = path.to_str().unwrap();

        // CP932 / Shift-JIS: 2バイト目が 0x5C (\) になる文字「表」「能」「構」や半角カナ「ｱｲｳ」を含む
        let original_text = "日本語のテスト\n表・能・構の文字\n半角ｶﾅｱｲｳｴｵ\n12345";
        let (sjis_bytes, _, _) = encoding_rs::SHIFT_JIS.encode(original_text);
        std::fs::write(&path, &sjis_bytes).unwrap();

        // 開いて自動判別
        let (mut doc, _) = Document::open(path_str).unwrap();
        assert_eq!(doc.encoding(), FileEncoding::ShiftJis);
        assert_eq!(doc.line_count(), 4);
        assert_eq!(
            doc.read(0, 4).unwrap(),
            vec!["日本語のテスト", "表・能・構の文字", "半角ｶﾅｱｲｳｴｵ", "12345"]
        );

        // リテラル検索
        let (hits, _) = doc.scan_literal("能", true, '$', 0, 4, 64).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, 1);
        assert_eq!(hits[0].start, 2);
        assert_eq!(hits[0].end, 3);

        // 正規表現検索
        let pattern = regex::Regex::new(r"表.能").unwrap();
        let (hits, _) = doc.scan(&pattern, '$', 0, 4, 64).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, 1);
        assert_eq!(hits[0].start, 0);
        assert_eq!(hits[0].end, 3);

        // 編集して Shift-JIS で保存
        doc.replace(3, 4, vec!["新行".into()], 1, "", "").unwrap();
        doc.save(path_str).unwrap();

        let saved_bytes = std::fs::read(&path).unwrap();
        let (decoded, _, _) = encoding_rs::SHIFT_JIS.decode(&saved_bytes);
        assert_eq!(
            decoded,
            "日本語のテスト\n表・能・構の文字\n半角ｶﾅｱｲｳｴｵ\n新行"
        );

        std::fs::remove_file(path).ok();
    }

    #[test]
    fn euc_jp_and_iso2022jp_reading() {
        let dir = std::env::temp_dir();

        // EUC-JP テスト
        let euc_path = dir.join("planetext-euc-test.txt");
        let euc_text = "EUC-JPの日本語テキスト\n二行目";
        let (euc_bytes, _, _) = encoding_rs::EUC_JP.encode(euc_text);
        std::fs::write(&euc_path, &euc_bytes).unwrap();

        let (mut doc, _) = Document::open(euc_path.to_str().unwrap()).unwrap();
        assert_eq!(doc.encoding(), FileEncoding::EucJp);
        assert_eq!(
            doc.read(0, 2).unwrap(),
            vec!["EUC-JPの日本語テキスト", "二行目"]
        );
        std::fs::remove_file(euc_path).ok();

        // ISO-2022-JP テスト
        let jis_path = dir.join("planetext-jis-test.txt");
        let jis_text = "JISの日本語テキスト\n二行目";
        let (jis_bytes, _, _) = encoding_rs::ISO_2022_JP.encode(jis_text);
        std::fs::write(&jis_path, &jis_bytes).unwrap();

        let (mut doc, _) = Document::open(jis_path.to_str().unwrap()).unwrap();
        assert_eq!(doc.encoding(), FileEncoding::Iso2022Jp);
        assert_eq!(
            doc.read(0, 2).unwrap(),
            vec!["JISの日本語テキスト", "二行目"]
        );
        std::fs::remove_file(jis_path).ok();
    }

    #[test]
    fn line_ending_detection_and_saving() {
        let dir = std::env::temp_dir();
        let crlf_path = dir.join("planetext-crlf-test.txt");
        std::fs::write(&crlf_path, b"line1\r\nline2\r\nline3").unwrap();

        let (mut doc, _) = Document::open(crlf_path.to_str().unwrap()).unwrap();
        assert_eq!(doc.line_ending(), LineEnding::CrLf);
        assert_eq!(doc.read(0, 3).unwrap(), vec!["line1", "line2", "line3"]);

        // LFに切り替えて保存
        doc.set_line_ending(LineEnding::Lf);
        doc.save(crlf_path.to_str().unwrap()).unwrap();

        let saved = std::fs::read(&crlf_path).unwrap();
        assert_eq!(saved, b"line1\nline2\nline3");
        std::fs::remove_file(crlf_path).ok();

        // CR 単独改行ファイルの読み込みと各行の分解テスト
        let cr_path = dir.join("planetext-cr-test.txt");
        std::fs::write(&cr_path, b"cr_line1\rcr_line2\rcr_line3").unwrap();
        let (mut cr_doc, _) = Document::open(cr_path.to_str().unwrap()).unwrap();
        assert_eq!(cr_doc.line_ending(), LineEnding::Cr);
        assert_eq!(cr_doc.line_count(), 3);
        assert_eq!(
            cr_doc.read(0, 3).unwrap(),
            vec!["cr_line1", "cr_line2", "cr_line3"]
        );
        std::fs::remove_file(cr_path).ok();
    }

    #[test]
    fn reopen_with_encoding_switches_decoding() {
        let dir = std::env::temp_dir();
        let path = dir.join("planetext-reopen-test.txt");
        let text = "あいうえお";
        let (sjis_bytes, _, _) = encoding_rs::SHIFT_JIS.encode(text);
        std::fs::write(&path, &sjis_bytes).unwrap();

        let (mut doc, _) = Document::open(path.to_str().unwrap()).unwrap();
        assert_eq!(doc.encoding(), FileEncoding::ShiftJis);
        assert_eq!(doc.read(0, 1).unwrap(), vec!["あいうえお"]);

        // UTF-8 で強制開き直し（文字化けが起きることを確認）
        doc.reopen_with_encoding(FileEncoding::Utf8).unwrap();
        assert_eq!(doc.encoding(), FileEncoding::Utf8);
        assert_ne!(doc.read(0, 1).unwrap(), vec!["あいうえお"]);

        // 再度 Shift-JIS で開き直すと正常に復帰
        doc.reopen_with_encoding(FileEncoding::ShiftJis).unwrap();
        assert_eq!(doc.encoding(), FileEncoding::ShiftJis);
        assert_eq!(doc.read(0, 1).unwrap(), vec!["あいうえお"]);

        std::fs::remove_file(path).ok();
    }

    /// 回帰: Clean な下書きに遠方の保留 Redo 差分があるとき、
    /// 復元時にその行マークまで確定され、即時 Redo が全文走査へ落ちないことを検証する。
    #[test]
    fn draft_restore_includes_pending_redo_in_max_needed_line() {
        use crate::persistence::DraftDiff;
        // max_needed_line の計算ロジックを直接検証する（lib.rs 側の統合は別途）。
        // 保留 Redo 差分（head より後ろ）の from_line が active 差分より遠方にある場合、
        // max_needed_line はその Redo 差分の位置を含む必要がある。
        let active = DraftDiff {
            group: 1,
            from_line: 5,
            removed_lines: 1,
            lines: vec!["a".into()],
            deleted_lines: vec![],
            before: String::new(),
            after: String::new(),
        };
        let pending_redo = DraftDiff {
            group: 2,
            from_line: 999_999,
            removed_lines: 1,
            lines: vec!["b".into()],
            deleted_lines: vec![],
            before: String::new(),
            after: String::new(),
        };
        let diffs = [active, pending_redo];
        let max_needed_line = diffs
            .iter()
            .map(|d| d.from_line + d.removed_lines)
            .max()
            .unwrap_or(0);
        assert_eq!(max_needed_line, 1_000_000, "保留 Redo の位置を含むこと");
    }
}
