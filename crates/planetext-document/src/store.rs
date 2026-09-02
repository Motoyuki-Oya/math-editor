#[cfg(test)]
mod tests {
    use crate::document::Document;
    use crate::search::{literal_positions, SearchSpec};
    use crate::source::{FileEncoding, LineEnding, CHUNK, MAX_LINE_BYTES, STRIDE};

    /// 一意な一時ファイルに行を書き、開いた文書とパスを返す。
    fn disk_doc(name: &str, lines: &[&str]) -> (Document, String) {
        let path = std::env::temp_dir().join(format!(
            "planetext-store-{}-{}.txt",
            std::process::id(),
            name
        ));
        let path = path.to_string_lossy().into_owned();
        std::fs::write(&path, lines.join("\n")).unwrap();
        let (mut doc, scan) = Document::open(&path).unwrap();
        if let Some(scan) = scan {
            scan.run().unwrap();
            doc.confirm_scan();
        }
        (doc, path)
    }

    fn all(doc: &mut Document) -> Vec<String> {
        doc.read(0, usize::MAX).unwrap()
    }

    fn utf16_file_bytes(text: &str, encoding: FileEncoding) -> Vec<u8> {
        let mut bytes = match encoding {
            FileEncoding::Utf16Le => b"\xFF\xFE".to_vec(),
            FileEncoding::Utf16Be => b"\xFE\xFF".to_vec(),
            _ => unreachable!(),
        };
        bytes.extend(text.encode_utf16().flat_map(|unit| match encoding {
            FileEncoding::Utf16Le => unit.to_le_bytes(),
            FileEncoding::Utf16Be => unit.to_be_bytes(),
            _ => unreachable!(),
        }));
        bytes
    }

    fn encoded_text(text: &str, encoding: FileEncoding) -> Vec<u8> {
        match encoding {
            FileEncoding::Utf16Le => text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
            FileEncoding::Utf16Be => text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
            _ => encoding.encode_str(text),
        }
    }

    fn encoded_disk_doc(
        name: &str,
        lines: &[&str],
        encoding: FileEncoding,
        line_ending: LineEnding,
        trailing_newline: bool,
    ) -> (Document, String) {
        let path =
            std::env::temp_dir().join(format!("planetext-store-{}-{name}.txt", std::process::id()));
        let separator = std::str::from_utf8(line_ending.as_bytes()).unwrap();
        let mut text = lines.join(separator);
        if trailing_newline {
            text.push_str(separator);
        }
        let bytes = match encoding {
            FileEncoding::Utf16Le | FileEncoding::Utf16Be => utf16_file_bytes(&text, encoding),
            _ => encoded_text(&text, encoding),
        };
        std::fs::write(&path, bytes).unwrap();
        let path = path.to_string_lossy().into_owned();
        let (doc, scan) = Document::open_with_encoding(&path, Some(encoding)).unwrap();
        assert!(scan.is_none());
        (doc, path)
    }

    #[test]
    fn opening_indexes_lines_without_holding_the_contents() {
        let (mut doc, path) = disk_doc("open", &["ab", "", "cd"]);
        assert_eq!(doc.line_count(), 3);
        assert_eq!(all(&mut doc), vec!["ab", "", "cd"]);
        assert_eq!(doc.read(2, 1).unwrap(), vec!["cd"]);
        std::fs::remove_file(path).ok();
    }

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
            assert_eq!(doc.source.as_ref().unwrap().content_offset, offset);
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

            assert_eq!(doc.source.as_ref().unwrap().content_offset, 0);
            assert_eq!(
                all(&mut doc),
                String::from_utf8_lossy(bytes).lines().collect::<Vec<_>>()
            );
            std::fs::remove_file(path).ok();
        }
    }

    #[test]
    fn background_scan_confirms_and_reads_the_empty_final_line() {
        let path = std::env::temp_dir().join(format!(
            "planetext-store-{}-background-final.txt",
            std::process::id()
        ));
        let line = "0123456789\n";
        let complete_lines = 100_000usize;
        std::fs::write(&path, line.repeat(complete_lines)).unwrap();
        let path = path.to_string_lossy().into_owned();
        let (mut doc, scan) = Document::open(&path).unwrap();
        assert!(scan.is_some());
        assert_eq!(doc.read_tail(2).unwrap(), vec!["0123456789", ""]);
        scan.unwrap().run().unwrap();
        doc.confirm_scan();
        assert_eq!(doc.line_count(), complete_lines + 1);
        assert_eq!(doc.read(complete_lines, 1).unwrap(), vec![""]);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn replacing_provisional_final_line_preserves_pending_source_suffix() {
        let path = std::env::temp_dir().join(format!(
            "planetext-store-{}-background-provisional-final.txt",
            std::process::id()
        ));
        let source_lines = 100_000usize;
        let source = (0..source_lines)
            .map(|line| format!("source line {line:06}\n"))
            .collect::<String>();
        std::fs::write(&path, &source).unwrap();
        let path = path.to_string_lossy().into_owned();
        let (mut doc, scan) = Document::open(&path).unwrap();
        let scan = scan.expect("the file must exceed the initial scan chunk");
        let provisional_line = doc.line_count() - 1;
        let original = doc.read(provisional_line, 1).unwrap();

        doc.replace(
            provisional_line,
            provisional_line + 1,
            vec!["edited provisional line".into()],
            1,
            "before",
            "after",
        )
        .unwrap();
        scan.run().unwrap();
        doc.confirm_scan();

        assert_eq!(doc.line_count(), source_lines + 1);
        assert_eq!(
            doc.read(provisional_line, 2).unwrap(),
            vec![
                "edited provisional line".to_string(),
                format!("source line {:06}", provisional_line + 1),
            ]
        );
        assert_eq!(
            doc.read(source_lines - 1, 2).unwrap(),
            vec![format!("source line {:06}", source_lines - 1), "".into()]
        );

        let undone = doc.undo().unwrap().unwrap();
        assert_eq!(undone.state, "before");
        assert_eq!(doc.line_count(), source_lines + 1);
        assert_eq!(doc.read(provisional_line, 1).unwrap(), original);
        assert_eq!(
            doc.read(source_lines - 1, 2).unwrap(),
            vec![format!("source line {:06}", source_lines - 1), "".into()]
        );
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn confirming_background_scan_preserves_prior_edits_and_history() {
        let path = std::env::temp_dir().join(format!(
            "planetext-store-{}-background-edited.txt",
            std::process::id()
        ));
        let source_line = "0123456789 source line\n";
        let source_lines = 60_000usize;
        let source_bytes = source_line.repeat(source_lines);
        std::fs::write(&path, &source_bytes).unwrap();
        let path = path.to_string_lossy().into_owned();
        let (mut doc, scan) = Document::open(&path).unwrap();
        let scan = scan.expect("the file must exceed the initial scan chunk");

        doc.replace(
            1,
            2,
            vec!["edited one".into(), "edited two".into()],
            1,
            "before",
            "after",
        )
        .unwrap();
        scan.run().unwrap();
        doc.confirm_scan();

        assert_eq!(doc.line_count(), source_lines + 2);
        assert_eq!(doc.pieces.line_count(), source_lines + 2);
        assert_eq!(doc.pieces.newline_count, source_lines + 1);
        assert_eq!(
            doc.bytes(),
            source_bytes.len() - "0123456789 source line".len() + "edited one\nedited two".len()
        );
        assert_eq!(
            doc.read(0, 4).unwrap(),
            vec![
                "0123456789 source line",
                "edited one",
                "edited two",
                "0123456789 source line"
            ]
        );

        let undone = doc.undo().unwrap().unwrap();
        assert_eq!(undone.state, "before");
        assert_eq!(doc.line_count(), source_lines + 1);
        assert_eq!(doc.pieces.newline_count, source_lines);
        assert_eq!(doc.bytes(), source_bytes.len());
        assert_eq!(doc.read(0, 3).unwrap(), vec!["0123456789 source line"; 3]);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn sparse_mark_at_eof_exposes_the_final_empty_line() {
        let path = std::env::temp_dir().join(format!(
            "planetext-store-{}-stride-final-empty.txt",
            std::process::id()
        ));
        std::fs::write(&path, "\n".repeat(STRIDE)).unwrap();
        let path = path.to_string_lossy().into_owned();
        let (mut doc, scan) =
            Document::open_with_encoding(&path, Some(FileEncoding::Utf8)).unwrap();

        assert!(scan.is_none());
        let source = doc.source.as_ref().unwrap();
        assert_eq!(source.index.state.lock().unwrap().marks[1], source.bytes);
        assert_eq!(doc.line_count(), STRIDE + 1);
        assert_eq!(doc.read(STRIDE, 1).unwrap(), vec![""]);
        std::fs::remove_file(path).ok();
    }

    /// 間引きの索引: STRIDE を越える文書でも、行は最寄りの索引から読み流して届く。
    #[test]
    fn far_lines_are_reached_through_the_sparse_index() {
        let lines: Vec<String> = (0..STRIDE * 2 + 5).map(|i| format!("line {i}")).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let (mut doc, path) = disk_doc("stride", &refs);
        assert_eq!(doc.line_count(), STRIDE * 2 + 5);
        assert_eq!(
            doc.read(STRIDE + 3, 2).unwrap(),
            vec![
                format!("line {}", STRIDE + 3),
                format!("line {}", STRIDE + 4)
            ]
        );
        assert_eq!(
            doc.read(STRIDE * 2 + 4, 5).unwrap(),
            vec![format!("line {}", STRIDE * 2 + 4)]
        );
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

        assert!(doc.log.transactions[0].edits()[0].removed.lines == 0);
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

    #[test]
    fn scanning_reports_matches_and_notation_lines_in_order() {
        let (mut doc, path) = disk_doc("scan", &["fox", "a$b", "the fox and fox", "last"]);
        let pattern = regex::Regex::new("fox").unwrap();
        let (hits, scanned_to) = doc.scan(&pattern, '$', 0, 10, 64).unwrap();
        assert_eq!(scanned_to, 4);
        let kinds: Vec<(usize, bool, usize, usize)> = hits
            .iter()
            .map(|hit| (hit.line, hit.notation, hit.start, hit.end))
            .collect();
        assert_eq!(
            kinds,
            vec![
                (0, false, 0, 3),
                (1, true, 0, 0),
                (2, false, 4, 7),
                (2, false, 12, 15),
            ]
        );
        // 途中から頼めば続きが返る。
        let (hits, _) = doc.scan(&pattern, '$', 2, 10, 64).unwrap();
        assert_eq!(hits.len(), 2);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn estimating_matches_is_exact_when_every_line_is_sampled() {
        let (mut doc, path) = disk_doc("estimate", &["hit hit", "none", "hit"]);
        let pattern = regex::Regex::new("hit").unwrap();
        assert_eq!(doc.estimate_matches(&pattern).unwrap(), 3);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn estimating_matches_extrapolates_uniform_samples() {
        let lines: Vec<String> = (0..200_000).map(|_| "hit".to_string()).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let (mut doc, path) = disk_doc("estimate-large", &refs);
        let pattern = regex::Regex::new("hit").unwrap();
        assert_eq!(doc.estimate_matches(&pattern).unwrap(), 200_000);
        std::fs::remove_file(path).ok();
    }

    /// 【回帰防止テスト】
    /// バックグラウンド走査中の巨大ファイル（pending_source が残る状態）であっても、
    /// estimate_matches は最初のチャンク行数で頭打ちにならず、ファイル全体のサイズに
    /// 正しく外挿された推定件数を返すことを保証する。
    /// また、走査が完了した後は自動的に confirm_scan_if_done が走り、確定件数が返ることを保証する。
    #[test]
    fn estimate_matches_extrapolates_pending_source_and_confirms_when_done() {
        use crate::document::PendingSource;
        use crate::edit_buffers::EditBuffers;
        use crate::operation_log::OperationLog;
        use crate::piece_tree::{Piece, PieceTree};

        let lines: Vec<String> = (0..100_000)
            .map(|i| format!("line {i} with keyword target"))
            .collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let (mut doc, path) = disk_doc("estimate-pending-extrapolate", &refs);
        let pattern = regex::Regex::new("target").unwrap();

        let initial_lines = 1_000;
        let initial_bytes = doc.pieces.byte_offset(initial_lines);
        let total_bytes = doc.pieces.byte_offset(100_000);

        let mut simulated_doc = Document {
            pieces: PieceTree::new(vec![
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
            ]),
            buffers: EditBuffers::default(),
            count: initial_lines,
            encoding: doc.encoding,
            line_ending: doc.line_ending,
            source: doc.source.take(),
            log: OperationLog::default(),
            pending_source: Some(PendingSource {
                from: initial_bytes,
                len: total_bytes - initial_bytes,
                prefix_newlines: initial_lines - 1,
            }),
            search_index: None,
            background_index: None,
        };

        // 未確定状態（count = 1,000）でも、ファイル全体規模（約100,000件）に外挿されること
        let estimated = simulated_doc.estimate_matches(&pattern).unwrap();
        assert!(
            (90_000..=110_000).contains(&estimated),
            "未確定走査中でもファイル全体規模に外挿されること: expected ~100000, got {estimated}"
        );

        // 走査完了をシミュレート
        if let Some(source) = &simulated_doc.source {
            let mut state = source.index.state.lock().unwrap();
            state.done = true;
            state.lines = 100_000;
        }

        // 走査完了後は confirm_scan_if_done が自動で走り、count が 100,000 に確定すること
        let final_estimated = simulated_doc.estimate_matches(&pattern).unwrap();
        assert!(
            (final_estimated as isize - 100_000).abs() <= 10,
            "走査完了後の推定件数は約100,000件であること: got {final_estimated}"
        );
        assert_eq!(simulated_doc.count, 100_000);

        std::fs::remove_file(path).ok();
    }

    #[test]
    fn ascii_case_fold_finds_non_overlapping_matches() {
        assert_eq!(literal_positions(b"xxAbCaBC", b"abc", false), vec![2, 5]);
        assert_eq!(literal_positions(b"aaaa", b"aa", false), vec![0, 2]);
        assert!(literal_positions(b"ABC", b"abc", true).is_empty());
    }

    #[test]
    fn ascii_case_insensitive_scan_keeps_utf8_columns() {
        let (mut doc, path) = disk_doc("ascii-fold", &["前AbC後aBc"]);
        let (hits, _) = doc.scan_literal("abc", false, '$', 0, 1, 64).unwrap();
        assert_eq!(
            hits.iter()
                .map(|hit| (hit.start, hit.end))
                .collect::<Vec<_>>(),
            vec![(1, 4), (5, 8)]
        );
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn search_snapshot_keeps_the_contents_at_search_start() {
        let (mut doc, path) = disk_doc("search-snapshot", &["before", "tail"]);
        doc.replace(0, 1, vec!["first".into()], 1, "", "").unwrap();
        let mut snapshot = doc.search_snapshot().unwrap();
        doc.replace(0, 1, vec!["second".into()], 2, "", "").unwrap();
        let query = crate::search::CompiledQuery::compile("first", false, true, '$').unwrap();
        let found = snapshot
            .search_candidates(
                SearchSpec {
                    query: &query,
                    from: 0,
                    end: 2,
                    after_col: None,
                },
                &|| false,
            )
            .unwrap();
        assert_eq!(found.hits.len(), 1);
        assert!(!found.cancelled);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn native_search_stops_when_cancelled() {
        let (doc, path) = disk_doc("search-cancel", &["a", "b"]);
        let mut snapshot = doc.search_snapshot().unwrap();
        let query = crate::search::CompiledQuery::compile("missing", false, true, '$').unwrap();
        let found = snapshot
            .search_candidates(
                SearchSpec {
                    query: &query,
                    from: 0,
                    end: 2,
                    after_col: None,
                },
                &|| true,
            )
            .unwrap();
        assert!(found.cancelled);
        assert!(found.hits.is_empty());
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn literal_search_candidates_resume_after_a_character_column() {
        let (doc, path) = disk_doc("literal-search-resume", &["前needle後needle", "needle"]);
        let mut snapshot = doc.search_snapshot().unwrap();
        let query = crate::search::CompiledQuery::compile("needle", false, true, '$').unwrap();
        let found = snapshot
            .search_candidates(
                SearchSpec {
                    query: &query,
                    from: 0,
                    end: 2,
                    after_col: Some(8),
                },
                &|| false,
            )
            .unwrap();

        assert!(!found.cancelled);
        assert_eq!(found.scanned_to, 2);
        assert_eq!(
            found
                .hits
                .iter()
                .map(|hit| (hit.line, hit.start, hit.end))
                .collect::<Vec<_>>(),
            vec![(0, 8, 14), (1, 0, 6)]
        );
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn literal_scan_reports_utf8_character_columns() {
        let (mut doc, path) = disk_doc("literal-scan", &["前abc後abc"]);
        let (hits, _) = doc.scan_literal("abc", true, '$', 0, 1, 64).unwrap();
        assert_eq!(
            hits.iter()
                .map(|hit| (hit.start, hit.end))
                .collect::<Vec<_>>(),
            vec![(1, 4), (5, 8)]
        );
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn literal_scan_handles_a_source_line_over_the_display_limit() {
        let prefix = "x".repeat(MAX_LINE_BYTES + 1);
        let line = format!("{prefix}needle");
        let (mut doc, path) = disk_doc("literal-over-display-limit", &[&line]);

        let (hits, scanned_to) = doc.scan_literal("needle", true, '$', 0, 1, 64).unwrap();

        assert_eq!(scanned_to, 1);
        assert_eq!(
            hits.iter()
                .map(|hit| (hit.line, hit.notation, hit.start, hit.end))
                .collect::<Vec<_>>(),
            vec![(0, false, MAX_LINE_BYTES + 1, MAX_LINE_BYTES + 7)]
        );
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn literal_scan_finds_a_match_across_its_source_chunk_boundary() {
        let line = format!("{}needle", "x".repeat(CHUNK - 3));
        let (mut doc, path) = disk_doc("literal-chunk-boundary", &[&line]);

        let (hits, _) = doc.scan_literal("needle", true, '$', 0, 1, 64).unwrap();

        assert_eq!(hits.len(), 1);
        assert_eq!(
            (hits[0].line, hits[0].start, hits[0].end),
            (0, CHUNK - 3, CHUNK + 3)
        );
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn literal_scan_keeps_utf16_alignment_and_character_columns() {
        for (encoding, alignment_line, alignment_query) in [
            (FileEncoding::Utf16Le, "\0\u{1}Ā", "Ā"),
            (FileEncoding::Utf16Be, "ĀĀ\u{1}", "\u{1}"),
        ] {
            let (mut doc, path) = encoded_disk_doc(
                &format!("literal-{encoding:?}"),
                &["前😀needle後", "needle", alignment_line],
                encoding,
                LineEnding::Lf,
                false,
            );

            let (hits, _) = doc.scan_literal("needle", true, '$', 0, 3, 64).unwrap();
            assert_eq!(
                hits.iter()
                    .map(|hit| (hit.line, hit.start, hit.end))
                    .collect::<Vec<_>>(),
                vec![(0, 2, 8), (1, 0, 6)]
            );
            let (aligned, _) = doc
                .scan_literal(alignment_query, true, '$', 2, 1, 64)
                .unwrap();
            assert_eq!(
                aligned
                    .iter()
                    .map(|hit| (hit.line, hit.start, hit.end))
                    .collect::<Vec<_>>(),
                vec![(2, 2, 3)]
            );
            std::fs::remove_file(path).ok();
        }
    }

    #[test]
    fn literal_scan_searches_edited_pieces_and_keeps_global_lines() {
        let (mut doc, path) = disk_doc("literal-edited-piece", &["zero", "one", "two", "three"]);
        doc.replace(
            1,
            3,
            vec![
                "edited needle".into(),
                "middle".into(),
                "needle tail".into(),
            ],
            1,
            "",
            "",
        )
        .unwrap();

        let (hits, _) = doc
            .scan_literal("needle", true, '$', 0, doc.line_count(), 64)
            .unwrap();

        assert_eq!(
            hits.iter()
                .map(|hit| (hit.line, hit.start, hit.end))
                .collect::<Vec<_>>(),
            vec![(1, 7, 13), (3, 0, 6)]
        );
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn notation_marker_suppresses_all_literal_hits_on_its_line() {
        let (mut doc, path) = disk_doc("literal-notation", &["needle $ needle", "needle"]);

        let (hits, _) = doc.scan_literal("needle", true, '$', 0, 2, 64).unwrap();

        assert_eq!(
            hits.iter()
                .map(|hit| (hit.line, hit.notation, hit.start, hit.end))
                .collect::<Vec<_>>(),
            vec![(0, true, 0, 0), (1, false, 0, 6)]
        );
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn literal_scan_small_limit_counts_a_marker_line_once() {
        let (mut doc, path) = disk_doc(
            "literal-small-limit-marker",
            &["needle $ needle", "前needle後needle", "needle"],
        );

        let (hits, scanned_to) = doc.scan_literal("needle", true, '$', 0, 3, 2).unwrap();

        assert_eq!(scanned_to, 2);
        assert_eq!(
            hits.iter()
                .map(|hit| (hit.line, hit.notation, hit.start, hit.end))
                .collect::<Vec<_>>(),
            vec![(0, true, 0, 0), (1, false, 1, 7)]
        );
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn literal_scan_line_numbers_are_correct_across_source_and_edit_pieces() {
        let (mut doc, path) = disk_doc(
            "literal-multiple-pieces",
            &["needle 0", "one", "two", "three", "needle 4"],
        );
        doc.replace(2, 3, vec!["needle 2".into()], 1, "", "")
            .unwrap();

        let (hits, scanned_to) = doc.scan_literal("needle", true, '$', 0, 5, 64).unwrap();

        assert_eq!(scanned_to, 5);
        assert_eq!(
            hits.iter().map(|hit| hit.line).collect::<Vec<_>>(),
            vec![0, 2, 4]
        );
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn finding_lines_that_contain_a_character() {
        let (mut doc, path) = disk_doc("contains", &["aa", "a$b", "cc", "$"]);
        assert_eq!(doc.lines_containing(0, 3, '$').unwrap(), vec![1, 3]);
        assert_eq!(doc.lines_containing(0, 99, '$').unwrap(), vec![1, 3]);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn multi_piece_callbacks_keep_global_line_numbers() {
        let (mut doc, path) = disk_doc("global-lines", &["a", "b", "c", "d", "e", "f"]);
        doc.replace(
            3,
            4,
            vec!["x".into(), "$one".into(), "y$".into()],
            1,
            "",
            "",
        )
        .unwrap();
        assert_eq!(doc.lines_containing(0, 99, '$').unwrap(), vec![4, 5]);
        let overrides = std::collections::HashMap::from([(5, "override".to_string())]);
        assert_eq!(
            doc.assemble(2, None, 6, None, &overrides).unwrap(),
            "c\nx\n$one\noverride\ne"
        );
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
            !doc.log.transactions.is_empty(),
            "小さいファイルは保存後も Undo 履歴が保持されること"
        );
        std::fs::remove_file(path).ok();
    }

    /// 【回帰防止テスト】
    /// 巨大ファイル（10MB超）の保存後、新しいベースへ切り替わり、メモリを圧迫しないよう
    /// 操作ログと編集バッファが解放されることを検証する。
    #[test]
    fn large_file_save_clears_log_and_switches_base() {
        let large_chunk = "B".repeat(1024 * 1024); // 1MB の行
        let lines: Vec<String> = vec![large_chunk.clone(); 11]; // 11MB 分
        let lines_ref: Vec<&str> = lines.iter().map(String::as_str).collect();
        let (mut doc, path) = disk_doc("large-save-clear", &lines_ref);

        doc.replace(1, 2, vec!["edited large line".into()], 1, "", "")
            .unwrap();
        assert!(!doc.log.transactions.is_empty());

        doc.save(&path).unwrap();
        assert!(doc.is_clean(), "保存後は clean になること");
        assert!(
            doc.log.transactions.is_empty(),
            "巨大ファイルは保存後に操作ログが破棄されてメモリが解放されること"
        );
        assert_eq!(doc.buffers.len(), 0, "編集バッファも解放されること");
        std::fs::remove_file(path).ok();
    }

    fn assert_document_state(doc: &mut Document, expected: &[String]) {
        assert_eq!(all(doc), expected);
        assert_eq!(doc.line_count(), expected.len());
        assert_eq!(doc.pieces.line_count(), expected.len());
        assert_eq!(doc.pieces.newline_count, expected.len().saturating_sub(1));
        let separator = std::str::from_utf8(doc.line_ending().as_bytes()).unwrap();
        assert_eq!(
            doc.bytes(),
            encoded_text(&expected.join(separator), doc.encoding()).len()
        );
        assert_eq!(doc.pieces.byte_len, doc.bytes());
    }

    fn assert_encoded_document_state(doc: &mut Document, expected: &[&str]) {
        let expected_lines: Vec<String> = expected.iter().map(|line| (*line).to_string()).collect();
        let separator = std::str::from_utf8(doc.line_ending().as_bytes()).unwrap();
        let text = expected.join(separator);
        assert_eq!(all(doc), expected_lines);
        assert_eq!(doc.line_count(), expected.len());
        assert_eq!(doc.pieces.line_count(), expected.len());
        assert_eq!(doc.pieces.newline_count, expected.len().saturating_sub(1));
        assert_eq!(doc.bytes(), encoded_text(&text, doc.encoding()).len());
        assert_eq!(doc.pieces.byte_len, doc.bytes());
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

    /// EOF seekによる末尾読みの実測。行数走査は開始しない。
    #[test]
    #[ignore]
    fn scale_check_reading_the_tail_without_line_count() {
        let path = r"C:\workspace\test-800mb.txt";
        if !std::path::Path::new(path).exists() {
            return;
        }
        let (mut doc, _scan) = Document::open(path).unwrap();
        let start = std::time::Instant::now();
        let tail = doc.read_tail(100).unwrap();
        println!("read tail: {:?} ({} lines)", start.elapsed(), tail.len());
        assert_eq!(tail.len(), 100);
    }

    /// 先頭1MBの同期読みと、残りの行数・間引き索引走査だけの実測。
    #[test]
    #[ignore]
    fn scale_check_counting_lines() {
        let path = r"C:\workspace\test-800mb.txt";
        if !std::path::Path::new(path).exists() {
            return;
        }
        let start = std::time::Instant::now();
        let (mut doc, scan) = Document::open(path).unwrap();
        let opened = start.elapsed();
        let start = std::time::Instant::now();
        if let Some(scan) = scan {
            scan.run().unwrap();
        }
        doc.confirm_scan();
        println!(
            "open: {opened:?}, scan: {:?}, lines: {}",
            start.elapsed(),
            doc.line_count()
        );
        assert_eq!(doc.line_count(), 16_000_001);
    }

    /// 規模の実測: `cargo test -p planetext --release -- --ignored --nocapture`。
    /// C:\workspace\test-800mb.txt がある環境でだけ動く。
    #[test]
    #[ignore]
    fn scale_check_opening_a_huge_file() {
        let path = r"C:\workspace\test-800mb.txt";
        if !std::path::Path::new(path).exists() {
            return;
        }
        let start = std::time::Instant::now();
        let (mut doc, scan) = Document::open(path).unwrap();
        println!(
            "open (scanned {} lines): {:?}",
            doc.line_count(),
            start.elapsed()
        );
        let start = std::time::Instant::now();
        if let Some(scan) = scan {
            scan.run().unwrap();
        }
        doc.confirm_scan();
        println!(
            "scan (exact {} lines): {:?}",
            doc.line_count(),
            start.elapsed()
        );
        let start = std::time::Instant::now();
        let middle = doc.read(doc.line_count() / 2, 100).unwrap();
        println!(
            "read 100 lines: {:?} ({} lines)",
            start.elapsed(),
            middle.len()
        );
        let missing = "planetext-not-present";
        let start = std::time::Instant::now();
        let _ = doc
            .scan_literal(missing, true, '$', 0, doc.line_count(), 64)
            .unwrap();
        println!("literal full scan: {:?}", start.elapsed());
        let start = std::time::Instant::now();
        let _ = doc
            .scan_literal("PLANETEXT-NOT-PRESENT", false, '$', 0, doc.line_count(), 64)
            .unwrap();
        println!("ASCII fold full scan: {:?}", start.elapsed());
        let mut snapshot = doc.search_snapshot().unwrap();
        let query = crate::search::CompiledQuery::compile(missing, false, true, '$').unwrap();
        let start = std::time::Instant::now();
        let searched = snapshot
            .search_candidates(
                SearchSpec {
                    query: &query,
                    from: 0,
                    end: doc.line_count(),
                    after_col: None,
                },
                &|| false,
            )
            .unwrap();
        println!(
            "single native job ({} hits): {:?}",
            searched.hits.len(),
            start.elapsed()
        );
        let start = std::time::Instant::now();
        let estimate = doc
            .estimate_matches(&regex::Regex::new("fox").unwrap())
            .unwrap();
        println!("estimate ({estimate} matches): {:?}", start.elapsed());
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
        let undo_len = doc.log.head;

        assert!(doc
            .replace(0, 5, Vec::new(), 2, "before delete", "after delete")
            .is_err());
        assert_eq!(doc.line_count(), 5);
        assert_eq!(doc.read(4, 1).unwrap().len(), 1);
        assert_eq!(doc.log.head, undo_len);
    }

    /// 巨大な行を含むファイルで MAX_READ_BYTES ガードが正しく働き、
    /// 1回の read で 20MB を超えずに安全に打ち切られることを検証する。
    #[test]
    fn huge_lines_are_capped_at_max_read_bytes() {
        let dir = std::env::temp_dir();
        let path = dir.join("planetext-huge-lines-test.txt");
        // 5MBの行を10行（合計50MB）書き込む
        {
            let file = std::fs::File::create(&path).unwrap();
            let mut writer = std::io::BufWriter::new(file);
            let big_line = "a".repeat(5 * 1024 * 1024);
            for i in 0..10 {
                use std::io::Write;
                if i > 0 {
                    writeln!(writer).unwrap();
                }
                write!(writer, "{big_line}").unwrap();
            }
        }
        let (mut doc, scan) = Document::open(path.to_str().unwrap()).unwrap();
        if let Some(scan) = scan {
            scan.run().unwrap();
        }
        doc.confirm_scan();
        assert_eq!(doc.line_count(), 10);
        // 10行読もうとしても、20MBガードにより最初の4〜5行で打ち切られる
        let lines = doc.read(0, 10).unwrap();
        assert!(lines.len() < 10);
        assert!(lines.len() >= 3);
        let total_read_bytes: usize = lines.iter().map(|l| l.len()).sum();
        assert!(total_read_bytes <= 25 * 1024 * 1024);
        std::fs::remove_file(path).ok();
    }

    /// 10万行の巨大ファイルを生成して開き、索引付け・途中行 seek 読みが正しく動くことを検証。
    #[test]
    fn opening_and_reading_large_many_lines_file() {
        let dir = std::env::temp_dir();
        let path = dir.join("planetext-100k-lines-test.txt");
        {
            let file = std::fs::File::create(&path).unwrap();
            let mut writer = std::io::BufWriter::new(file);
            for i in 0..100_000 {
                use std::io::Write;
                if i > 0 {
                    writeln!(writer).unwrap();
                }
                write!(writer, "line {i} with some content").unwrap();
            }
        }
        let (mut doc, scan) = Document::open(path.to_str().unwrap()).unwrap();
        if let Some(scan) = scan {
            scan.run().unwrap();
        }
        doc.confirm_scan();
        assert_eq!(doc.line_count(), 100_000);
        // 先頭、中間、末尾の行を正確にseek読みできるか
        let middle = doc.read(50_000, 3).unwrap();
        assert_eq!(
            middle,
            vec![
                "line 50000 with some content",
                "line 50001 with some content",
                "line 50002 with some content"
            ]
        );
        let tail = doc.read(99_998, 2).unwrap();
        assert_eq!(
            tail,
            vec![
                "line 99998 with some content",
                "line 99999 with some content"
            ]
        );
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
            assert_eq!(doc.source.as_ref().unwrap().content_offset, 2);
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

    #[test]
    fn search_snapshot_maps_hits_across_intervening_edits() {
        let (mut doc, path) = disk_doc("search-map", &["apple", "banana", "cherry"]);
        let snap = doc.search_snapshot().unwrap();
        // 検索スナップショット取得後に先頭に 1 行挿入
        doc.replace(0, 0, vec!["prefix".to_string()], 1, "", "")
            .unwrap();
        assert_eq!(doc.line_count(), 4);

        // スナップショット時点では "cherry" は 2 行目
        let hits = vec![crate::search::ScanHit {
            line: 2,
            notation: false,
            start: 0,
            end: 6,
        }];
        let mapped = doc.map_search_hits(&snap, hits);
        assert_eq!(mapped.len(), 1);
        // 現在の文書では 3 行目へ写像されていること
        assert_eq!(mapped[0].line, 3);
        assert_eq!(mapped[0].start, 0);
        assert_eq!(mapped[0].end, 6);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn search_snapshot_invalidates_hits_overlapping_edits() {
        let (mut doc, path) = disk_doc("search-invalid", &["apple", "banana", "cherry"]);
        let snap = doc.search_snapshot().unwrap();
        // 検索スナップショット取得後に "banana"（1行目）を削除
        doc.replace(1, 2, Vec::new(), 1, "", "").unwrap();
        assert_eq!(doc.line_count(), 2);

        // スナップショット時点での "banana" ヒット
        let hits = vec![crate::search::ScanHit {
            line: 1,
            notation: false,
            start: 0,
            end: 6,
        }];
        let mapped = doc.map_search_hits(&snap, hits);
        // 削除範囲と重なっているため無効化されること
        assert!(mapped.is_empty());
        std::fs::remove_file(path).ok();
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

    #[test]
    fn search_index_estimates_matches_and_updates_on_edit() {
        let (mut doc, path) = disk_doc(
            "index-test",
            &["apple banana", "orange apple", "grape apple"],
        );
        doc.enable_search_index();

        // 初期状態で "apple" が 3 件推定されること
        let re = regex::Regex::new("apple").unwrap();
        assert_eq!(doc.estimate_matches(&re).unwrap(), 3);

        // 1行目を置換して "apple" を減らす
        doc.replace(
            0,
            1,
            vec!["kiwi banana".to_string()],
            1,
            "apple banana",
            "kiwi banana",
        )
        .unwrap();

        // 差分キャッシュにより推定件数が 2 件になること
        assert_eq!(doc.estimate_matches(&re).unwrap(), 2);

        std::fs::remove_file(path).ok();
    }

    #[test]
    fn search_index_treats_structure_format_as_raw_text() {
        // 構造フォーマット・数式記法を含むテキストもフィルタリングせずそのまま bi-gram インデックス化されること
        let (mut doc, path) = disk_doc(
            "index-notation-test",
            &["formula: $(E = mc^2)$", "text line", "another $(x + y)$"],
        );
        doc.enable_search_index();

        let re = regex::Regex::new(r"\$\(").unwrap();
        // 記法プレフィックス "$(" を含む箇所が 2 件推定されること
        assert_eq!(doc.estimate_matches(&re).unwrap(), 2);

        std::fs::remove_file(path).ok();
    }

    #[test]
    fn search_index_undo_redo_updates_deltas() {
        let (mut doc, path) = disk_doc(
            "index-undo-test",
            &["apple banana", "orange apple", "grape apple"],
        );
        doc.enable_search_index();

        let re = regex::Regex::new("apple").unwrap();
        assert_eq!(doc.estimate_matches(&re).unwrap(), 3);

        // 1行目を置換
        doc.replace(
            0,
            1,
            vec!["kiwi banana".to_string()],
            1,
            "apple banana",
            "kiwi banana",
        )
        .unwrap();
        assert_eq!(doc.estimate_matches(&re).unwrap(), 2);

        // undo で差分キャッシュが元に戻り、3 件になること
        doc.undo().unwrap();
        assert_eq!(doc.estimate_matches(&re).unwrap(), 3);

        // redo で再度 2 件になること
        doc.redo().unwrap();
        assert_eq!(doc.estimate_matches(&re).unwrap(), 2);

        std::fs::remove_file(path).ok();
    }

    #[test]
    fn search_candidates_finds_distant_matches_efficiently() {
        use crate::search::{CompiledQuery, SearchSpec};

        // 512KB を超える複数ブロックのファイルを作成し、遠いブロックのみにターゲットを配置
        let mut lines = Vec::new();
        // 1行約100バイト * 15,000行 = 約1.5MB（約3ブロック分）
        let padding = "x".repeat(95);
        for _ in 0..15_000 {
            lines.push(padding.clone());
        }
        lines.push("hello target_needle world".to_string());
        lines.push(padding);

        let lines_ref: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        let (mut doc, path) = disk_doc("distant-block-test", &lines_ref);
        doc.enable_search_index();

        let compiled = CompiledQuery::compile("target_needle", false, true, '\0').unwrap();
        let spec = SearchSpec {
            query: &compiled,
            from: 0,
            end: doc.line_count(),
            after_col: None,
        };
        let candidates = doc.search_candidates(spec, &|| false).unwrap();
        assert!(!candidates.cancelled);
        assert_eq!(candidates.hits.len(), 1);
        assert_eq!(candidates.hits[0].line, 15_000);
        assert_eq!(candidates.hits[0].start, 6);

        std::fs::remove_file(path).ok();
    }
}
