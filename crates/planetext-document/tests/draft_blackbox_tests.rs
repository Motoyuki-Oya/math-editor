use planetext_document::Application;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn unique_timestamp() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

struct TestContext {
    dir: PathBuf,
}

impl TestContext {
    fn new(prefix: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("{prefix}_{}", unique_timestamp()));
        fs::create_dir_all(&dir).unwrap();
        Self { dir }
    }

    fn file_path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    fn config_dir(&self) -> Option<PathBuf> {
        Some(self.dir.clone())
    }
}

impl Drop for TestContext {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn test_blackbox_clean_referenced_file() {
    let ctx = TestContext::new("planetext_bb_clean");
    let file_path = ctx.file_path("clean.txt");
    fs::write(&file_path, "line 1\nline 2\nline 3\n").unwrap();

    let app = Application::default();
    let doc = app
        .open_document(file_path.to_str().unwrap().to_string())
        .unwrap();

    // 未編集（Clean）のまま下書き保存
    app.save_draft(
        ctx.config_dir(),
        doc.handle,
        "1".into(),
        Some(file_path.to_str().unwrap().into()),
    )
    .unwrap();

    // 復元
    let reopened = app.open_draft(ctx.config_dir(), "1".into()).unwrap();
    let read = app.read_lines(reopened.handle, 0, 10).unwrap();
    assert_eq!(read.lines, vec!["line 1", "line 2", "line 3", ""]);
}

#[test]
fn test_blackbox_single_line_edit() {
    let ctx = TestContext::new("planetext_bb_single");
    let file_path = ctx.file_path("single.txt");
    fs::write(&file_path, "alpha\nbeta\ngamma\n").unwrap();

    let app = Application::default();
    let doc = app
        .open_document(file_path.to_str().unwrap().to_string())
        .unwrap();

    // 2行目を編集
    app.replace_lines(
        doc.handle,
        1,
        2,
        vec!["BETA_EDITED".into()],
        1,
        "".into(),
        "".into(),
    )
    .unwrap();

    app.save_draft(
        ctx.config_dir(),
        doc.handle,
        "2".into(),
        Some(file_path.to_str().unwrap().into()),
    )
    .unwrap();

    let reopened = app.open_draft(ctx.config_dir(), "2".into()).unwrap();
    let read = app.read_lines(reopened.handle, 0, 10).unwrap();
    assert_eq!(read.lines, vec!["alpha", "BETA_EDITED", "gamma", ""]);
}

#[test]
fn test_blackbox_head_and_tail_insert_delete() {
    let ctx = TestContext::new("planetext_bb_headtail");
    let file_path = ctx.file_path("headtail.txt");
    fs::write(&file_path, "row 0\nrow 1\nrow 2\nrow 3\n").unwrap();

    let app = Application::default();
    let doc = app
        .open_document(file_path.to_str().unwrap().to_string())
        .unwrap();

    // 先頭行（row 0）の前に1行挿入
    app.replace_lines(
        doc.handle,
        0,
        0,
        vec!["NEW HEAD".into()],
        1,
        "".into(),
        "".into(),
    )
    .unwrap();
    // row 2（現在のインデックス 3）を削除
    app.replace_lines(doc.handle, 3, 4, vec![], 2, "".into(), "".into())
        .unwrap();
    // 末尾に1行追加（現在の行数は 4）
    app.replace_lines(
        doc.handle,
        4,
        4,
        vec!["NEW TAIL".into()],
        3,
        "".into(),
        "".into(),
    )
    .unwrap();

    app.save_draft(
        ctx.config_dir(),
        doc.handle,
        "3".into(),
        Some(file_path.to_str().unwrap().into()),
    )
    .unwrap();

    let reopened = app.open_draft(ctx.config_dir(), "3".into()).unwrap();
    let read = app.read_lines(reopened.handle, 0, 10).unwrap();
    assert_eq!(
        read.lines,
        vec!["NEW HEAD", "row 0", "row 1", "row 3", "NEW TAIL", ""]
    );
}

#[test]
fn test_blackbox_consecutive_keystrokes_and_undo() {
    let ctx = TestContext::new("planetext_bb_undo");
    let file_path = ctx.file_path("undo.txt");
    fs::write(&file_path, "first\nsecond\nthird\n").unwrap();

    let app = Application::default();
    let doc = app
        .open_document(file_path.to_str().unwrap().to_string())
        .unwrap();

    // 文字タイピングをシミュレート
    app.replace_lines(doc.handle, 1, 2, vec!["s".into()], 1, "".into(), "".into())
        .unwrap();
    app.replace_lines(doc.handle, 1, 2, vec!["se".into()], 2, "".into(), "".into())
        .unwrap();
    app.replace_lines(
        doc.handle,
        1,
        2,
        vec!["sec".into()],
        3,
        "".into(),
        "".into(),
    )
    .unwrap();

    // 最後の操作を取り消し (Undo)
    app.undo_lines(doc.handle, false).unwrap();

    app.save_draft(
        ctx.config_dir(),
        doc.handle,
        "4".into(),
        Some(file_path.to_str().unwrap().into()),
    )
    .unwrap();

    let reopened = app.open_draft(ctx.config_dir(), "4".into()).unwrap();
    let read = app.read_lines(reopened.handle, 0, 10).unwrap();
    // "sec" ではなく Undo 後の "se" が復元されること
    assert_eq!(read.lines, vec!["first", "se", "third", ""]);
}

#[test]
fn test_blackbox_unicode_and_crlf() {
    let ctx = TestContext::new("planetext_bb_unicode");
    let file_path = ctx.file_path("unicode.txt");
    // CRLF 改行コードと日本語・絵文字
    let content = "こんにちは世界\r\nエディタテスト😊\r\n数式 $E=mc^2$\r\n";
    fs::write(&file_path, content).unwrap();

    let app = Application::default();
    let doc = app
        .open_document(file_path.to_str().unwrap().to_string())
        .unwrap();

    app.replace_lines(
        doc.handle,
        1,
        2,
        vec!["編集されたエディタテスト🚀🎉".into()],
        1,
        "".into(),
        "".into(),
    )
    .unwrap();

    app.save_draft(
        ctx.config_dir(),
        doc.handle,
        "5".into(),
        Some(file_path.to_str().unwrap().into()),
    )
    .unwrap();

    let reopened = app.open_draft(ctx.config_dir(), "5".into()).unwrap();
    let read = app.read_lines(reopened.handle, 0, 10).unwrap();
    assert_eq!(
        read.lines,
        vec![
            "こんにちは世界",
            "編集されたエディタテスト🚀🎉",
            "数式 $E=mc^2$",
            ""
        ]
    );
}

#[test]
fn test_blackbox_multiple_documents() {
    let ctx = TestContext::new("planetext_bb_multidoc");
    let file_a = ctx.file_path("doc_a.txt");
    let file_b = ctx.file_path("doc_b.txt");
    fs::write(&file_a, "File A content\n").unwrap();
    fs::write(&file_b, "File B content\n").unwrap();

    let app = Application::default();
    let doc_a = app
        .open_document(file_a.to_str().unwrap().to_string())
        .unwrap();
    let doc_b = app
        .open_document(file_b.to_str().unwrap().to_string())
        .unwrap();

    app.replace_lines(
        doc_a.handle,
        0,
        1,
        vec!["File A MODIFIED".into()],
        1,
        "".into(),
        "".into(),
    )
    .unwrap();
    app.replace_lines(
        doc_b.handle,
        0,
        1,
        vec!["File B MODIFIED".into()],
        2,
        "".into(),
        "".into(),
    )
    .unwrap();

    // それぞれ独立したIDで下書き保存
    app.save_draft(
        ctx.config_dir(),
        doc_a.handle,
        "100".into(),
        Some(file_a.to_str().unwrap().into()),
    )
    .unwrap();
    app.save_draft(
        ctx.config_dir(),
        doc_b.handle,
        "200".into(),
        Some(file_b.to_str().unwrap().into()),
    )
    .unwrap();

    // それぞれ復元して内容が互いに干渉していないことを検証
    let reopened_a = app.open_draft(ctx.config_dir(), "100".into()).unwrap();
    let reopened_b = app.open_draft(ctx.config_dir(), "200".into()).unwrap();

    let read_a = app.read_lines(reopened_a.handle, 0, 5).unwrap();
    let read_b = app.read_lines(reopened_b.handle, 0, 5).unwrap();

    assert_eq!(read_a.lines, vec!["File A MODIFIED", ""]);
    assert_eq!(read_b.lines, vec!["File B MODIFIED", ""]);
}

#[test]
fn test_blackbox_deleted_original_file_error() {
    let ctx = TestContext::new("planetext_bb_deleted");
    let file_path = ctx.file_path("will_delete.txt");
    fs::write(&file_path, "some line\n").unwrap();

    let app = Application::default();
    let doc = app
        .open_document(file_path.to_str().unwrap().to_string())
        .unwrap();
    app.replace_lines(
        doc.handle,
        0,
        1,
        vec!["modified".into()],
        1,
        "".into(),
        "".into(),
    )
    .unwrap();

    app.save_draft(
        ctx.config_dir(),
        doc.handle,
        "99".into(),
        Some(file_path.to_str().unwrap().into()),
    )
    .unwrap();

    // 元ファイルをディスク上から物理削除
    fs::remove_file(&file_path).unwrap();

    // open_draft がパニックせず適切な Err を返すこと
    let result = app.open_draft(ctx.config_dir(), "99".into());
    assert!(result.is_err(), "元ファイルがない場合は Err になるべき");
}

#[test]
fn test_blackbox_untitled_document() {
    let ctx = TestContext::new("planetext_bb_untitled");
    let app = Application::default();

    // 無題ドキュメントの新規作成
    let doc = app.create_document();
    app.replace_lines(
        doc.handle,
        0,
        1,
        vec!["Untitled line 1".into(), "Untitled line 2".into()],
        1,
        "".into(),
        "".into(),
    )
    .unwrap();

    // path: None で下書き保存
    app.save_draft(ctx.config_dir(), doc.handle, "50".into(), None)
        .unwrap();

    // 復元
    let reopened = app.open_draft(ctx.config_dir(), "50".into()).unwrap();
    let read = app.read_lines(reopened.handle, 0, 5).unwrap();
    assert_eq!(read.lines, vec!["Untitled line 1", "Untitled line 2"]);
}

#[test]
fn test_blackbox_stepwise_undo_and_highlight_decay() {
    let ctx = TestContext::new("planetext_bb_highlight_decay");
    let file_path = ctx.file_path("decay.txt");
    fs::write(
        &file_path,
        "line 0\nline 1\nline 2\nline 3\nline 4\nline 5\n",
    )
    .unwrap();

    let app = Application::default();
    let doc = app
        .open_document(file_path.to_str().unwrap().to_string())
        .unwrap();

    // 1行目を編集
    app.replace_lines(
        doc.handle,
        1,
        2,
        vec!["MODIFIED 1".into()],
        1,
        "".into(),
        "".into(),
    )
    .unwrap();
    // 5行目を編集
    app.replace_lines(
        doc.handle,
        5,
        6,
        vec!["MODIFIED 5".into()],
        2,
        "".into(),
        "".into(),
    )
    .unwrap();

    // 下書き保存
    app.save_draft(
        ctx.config_dir(),
        doc.handle,
        "77".into(),
        Some(file_path.to_str().unwrap().into()),
    )
    .unwrap();

    // 復元
    let reopened = app.open_draft(ctx.config_dir(), "77".into()).unwrap();

    // 1回目の Undo（5行目の編集を取り消す）
    let undo1 = app
        .undo_lines(reopened.handle, false)
        .unwrap()
        .expect("undo 1");
    assert_eq!(
        undo1.modified_lines,
        vec![1],
        "5行目Undo後は1行目のみハイライト"
    );
    assert!(!undo1.clean, "1行目が残っているので未保存（Dirty）");

    // 2回目の Undo（1行目の編集を取り消す）
    let undo2 = app
        .undo_lines(reopened.handle, false)
        .unwrap()
        .expect("undo 2");
    assert_eq!(
        undo2.modified_lines,
        Vec::<usize>::new(),
        "1行目Undo後はハイライトなし"
    );
    assert!(undo2.clean, "完全に最初に戻ったので Clean");

    // 元ファイルと完全同一になっていることを検証
    let read = app.read_lines(reopened.handle, 0, 7).unwrap();
    assert_eq!(
        read.lines,
        vec!["line 0", "line 1", "line 2", "line 3", "line 4", "line 5", ""]
    );
}

#[test]
fn test_blackbox_typing_chunk_undo_coalescing() {
    let ctx = TestContext::new("planetext_bb_typing_coalesce");
    let file_path = ctx.file_path("coalesce.txt");
    fs::write(&file_path, "base line\n").unwrap();

    let app = Application::default();
    let doc = app
        .open_document(file_path.to_str().unwrap().to_string())
        .unwrap();

    // 連続タイピング（すべて同じグループ group: 10 で「H」「He」「Hel」「Hell」「Hello」と入力）
    app.replace_lines(doc.handle, 0, 1, vec!["H".into()], 10, "".into(), "".into())
        .unwrap();
    app.replace_lines(
        doc.handle,
        0,
        1,
        vec!["He".into()],
        10,
        "".into(),
        "".into(),
    )
    .unwrap();
    app.replace_lines(
        doc.handle,
        0,
        1,
        vec!["Hel".into()],
        10,
        "".into(),
        "".into(),
    )
    .unwrap();
    app.replace_lines(
        doc.handle,
        0,
        1,
        vec!["Hell".into()],
        10,
        "".into(),
        "".into(),
    )
    .unwrap();
    app.replace_lines(
        doc.handle,
        0,
        1,
        vec!["Hello".into()],
        10,
        "".into(),
        "".into(),
    )
    .unwrap();

    // 下書き保存
    app.save_draft(
        ctx.config_dir(),
        doc.handle,
        "88".into(),
        Some(file_path.to_str().unwrap().into()),
    )
    .unwrap();

    // 下書き復元
    let reopened = app.open_draft(ctx.config_dir(), "88".into()).unwrap();
    let read_restored = app.read_lines(reopened.handle, 0, 2).unwrap();
    assert_eq!(read_restored.lines[0], "Hello");

    // 復元後、たった 1 回の Undo で "Hello" の全文字入力が一気に巻き戻り、Clean に戻ること！
    let undo = app
        .undo_lines(reopened.handle, false)
        .unwrap()
        .expect("single undo");
    assert!(undo.clean, "1回のUndoでタイピング全体が戻り、Cleanになる");
    assert_eq!(
        undo.modified_lines,
        Vec::<usize>::new(),
        "変更マークも完全に消去"
    );

    let read_after_undo = app.read_lines(reopened.handle, 0, 2).unwrap();
    assert_eq!(
        read_after_undo.lines[0], "base line",
        "元ファイルのテキストに一撃で復帰"
    );
}

#[test]
fn test_blackbox_cursor_restored_on_undo_after_draft_replay() {
    let ctx = TestContext::new("planetext_bb_cursor_undo");
    let file_path = ctx.file_path("cursor_undo.txt");
    fs::write(&file_path, "row 0\nrow 1\nrow 2\nrow 3\nrow 4\nrow 5\n").unwrap();

    let app = Application::default();
    let doc = app
        .open_document(file_path.to_str().unwrap().to_string())
        .unwrap();

    // 4行目を編集（編集前のカーソル位置 "4.2-4.2"、編集後 "4.10-4.10"）
    app.replace_lines(
        doc.handle,
        4,
        5,
        vec!["row 4 EDITED".into()],
        1,
        "4.2-4.2".into(),
        "4.10-4.10".into(),
    )
    .unwrap();

    // 下書き保存
    app.save_draft(
        ctx.config_dir(),
        doc.handle,
        "999".into(),
        Some(file_path.to_str().unwrap().into()),
    )
    .unwrap();

    // 下書き復元
    let reopened = app.open_draft(ctx.config_dir(), "999".into()).unwrap();

    // Undo 実行！
    let undo = app
        .undo_lines(reopened.handle, false)
        .unwrap()
        .expect("undo");

    // 復元後であっても、Undo 時に編集前のカーソル位置 "4.2-4.2" が正確に返されること！
    assert_eq!(
        undo.state, "4.2-4.2",
        "Undo時に編集前のカーソル位置へピョンと戻る"
    );
    assert_eq!(undo.touched_from, 4, "影響を受けた行は4行目");
    assert!(undo.clean, "元ファイルの状態に戻ったのでClean");
}

#[test]
fn test_blackbox_restore_with_undo_and_redo_replay() {
    let ctx = TestContext::new("test_redo");
    let file_path = ctx.file_path("test_redo.txt");
    fs::write(&file_path, "line 0\nline 1\nline 2\nline 3\n").unwrap();

    let app = Application::default();
    let doc = app
        .open_document(file_path.to_str().unwrap().to_string())
        .unwrap();

    // 1行目を編集
    app.replace_lines(
        doc.handle,
        1,
        2,
        vec!["line 1 EDITED".into()],
        1,
        "".into(),
        "".into(),
    )
    .unwrap();
    // 2行目を編集
    app.replace_lines(
        doc.handle,
        2,
        3,
        vec!["line 2 EDITED".into()],
        2,
        "".into(),
        "".into(),
    )
    .unwrap();

    // 1回 Undo して 2行目を取り消す（1行目だけ編集された状態）
    app.undo_lines(doc.handle, false).unwrap();
    assert_eq!(
        app.read_lines(doc.handle, 1, 2).unwrap().lines,
        vec!["line 1 EDITED", "line 2"]
    );

    // この Undo 状態で下書き保存！
    app.save_draft(
        ctx.config_dir(),
        doc.handle,
        "101".into(),
        Some(file_path.to_str().unwrap().into()),
    )
    .unwrap();

    // 復元！
    let reopened = app.open_draft(ctx.config_dir(), "101".into()).unwrap();
    // 復元直後は Undo された状態（1行目だけ編集済み）
    assert_eq!(
        app.read_lines(reopened.handle, 1, 2).unwrap().lines,
        vec!["line 1 EDITED", "line 2"]
    );

    // ★復元後に Redo 実行！2行目の編集が復活すること！
    let redo = app
        .undo_lines(reopened.handle, true)
        .unwrap()
        .expect("redo");
    assert_eq!(
        app.read_lines(reopened.handle, 1, 2).unwrap().lines,
        vec!["line 1 EDITED", "line 2 EDITED"]
    );
    assert_eq!(redo.modified_lines, vec![1, 2]);

    // さらに Undo 2回で Clean な状態に戻す
    app.undo_lines(reopened.handle, false).unwrap();
    let undo2 = app.undo_lines(reopened.handle, false).unwrap().unwrap();
    assert!(undo2.clean);
    assert_eq!(
        app.read_lines(reopened.handle, 1, 2).unwrap().lines,
        vec!["line 1", "line 2"]
    );

    // Clean な状態（ただし Redo 履歴は残っている）で再度下書き保存！
    app.save_draft(
        ctx.config_dir(),
        reopened.handle,
        "102".into(),
        Some(file_path.to_str().unwrap().into()),
    )
    .unwrap();

    // 再度復元！
    let drafts = app.read_drafts(ctx.config_dir());
    let draft = drafts.iter().find(|d| d.id == "102").unwrap();
    assert!(draft.clean, "下書きメタデータが Clean であること");

    let reopened_clean = app.open_draft(ctx.config_dir(), "102".into()).unwrap();
    assert!(reopened_clean.clean, "復元された文書が Clean であること");
    assert_eq!(
        app.read_lines(reopened_clean.handle, 1, 2).unwrap().lines,
        vec!["line 1", "line 2"]
    );

    // ★Clean な状態で復元しても、Redo で元の編集を 1 段階ずつ再開できること！
    app.undo_lines(reopened_clean.handle, true)
        .unwrap()
        .expect("redo 1");
    assert_eq!(
        app.read_lines(reopened_clean.handle, 1, 2).unwrap().lines,
        vec!["line 1 EDITED", "line 2"]
    );

    app.undo_lines(reopened_clean.handle, true)
        .unwrap()
        .expect("redo 2");
    assert_eq!(
        app.read_lines(reopened_clean.handle, 1, 2).unwrap().lines,
        vec!["line 1 EDITED", "line 2 EDITED"]
    );

    // ★同じ group 番号で複数の edit（例: 連続タイピング）を行った場合の一括 Redo 検証
    app.replace_lines(
        reopened_clean.handle,
        1,
        2,
        vec!["line 1 BATCH".into()],
        3,
        "".into(),
        "".into(),
    )
    .unwrap();
    app.replace_lines(
        reopened_clean.handle,
        2,
        3,
        vec!["line 2 BATCH".into()],
        3, // 同じグループ番号 3
        "".into(),
        "".into(),
    )
    .unwrap();

    // 1 回の Undo でグループ 3 の両方が一気に Undo されること
    app.undo_lines(reopened_clean.handle, false).unwrap();
    assert_eq!(
        app.read_lines(reopened_clean.handle, 1, 2).unwrap().lines,
        vec!["line 1 EDITED", "line 2 EDITED"]
    );

    // 下書き保存して復元
    app.save_draft(
        ctx.config_dir(),
        reopened_clean.handle,
        "103".into(),
        Some(file_path.to_str().unwrap().into()),
    )
    .unwrap();

    let reopened_batch = app.open_draft(ctx.config_dir(), "103".into()).unwrap();
    // 復元後、たった 1 回の Redo でグループ 3 の両方の編集が一括で復活すること！
    app.undo_lines(reopened_batch.handle, true)
        .unwrap()
        .expect("redo batch group");
    assert_eq!(
        app.read_lines(reopened_batch.handle, 1, 2).unwrap().lines,
        vec!["line 1 BATCH", "line 2 BATCH"]
    );
}
