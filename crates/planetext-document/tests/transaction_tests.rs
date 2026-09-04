use std::fs;
use planetext_document::FileTransaction;

#[test]
fn test_single_file_commit() {
    let temp_dir = std::env::temp_dir().join(format!("tx_test_single_{}", std::process::id()));
    fs::create_dir_all(&temp_dir).unwrap();

    let target = temp_dir.join("test.txt");
    fs::write(&target, "initial content").unwrap();

    let mut tx = FileTransaction::begin(&temp_dir).unwrap();
    tx.add_file_bytes(&target, b"updated content").unwrap();

    // コミット前: target はまだ古い内容で、.tmp が存在する
    assert_eq!(fs::read_to_string(&target).unwrap(), "initial content");
    assert!(temp_dir.join("test.txt.tmp").exists());

    tx.commit().unwrap();

    // コミット後: target が新しい内容になり、.tmp や .bak、ジャーナルは消去
    assert_eq!(fs::read_to_string(&target).unwrap(), "updated content");
    assert!(!temp_dir.join("test.txt.tmp").exists());
    assert!(!temp_dir.join("test.txt.bak").exists());
    assert!(!temp_dir.join(".tx_journal").exists());

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_multi_file_atomic_commit() {
    let temp_dir = std::env::temp_dir().join(format!("tx_test_multi_{}", std::process::id()));
    fs::create_dir_all(&temp_dir).unwrap();

    let file_a = temp_dir.join("A.txt");
    let file_b = temp_dir.join("B.json");
    let file_c = temp_dir.join("C.toml");

    fs::write(&file_a, "A initial").unwrap();
    fs::write(&file_b, r#"{"b": 1}"#).unwrap();
    // file_c は新規作成（事前ファイルなし）

    let mut tx = FileTransaction::begin(&temp_dir).unwrap();
    tx.add_file_bytes(&file_a, b"A updated").unwrap();
    tx.add_file_bytes(&file_b, br#"{"b": 2}"#).unwrap();
    tx.add_file_bytes(&file_c, b"c = 3").unwrap();

    // コミット前
    assert_eq!(fs::read_to_string(&file_a).unwrap(), "A initial");
    assert_eq!(fs::read_to_string(&file_b).unwrap(), r#"{"b": 1}"#);
    assert!(!file_c.exists());

    tx.commit().unwrap();

    // コミット後
    assert_eq!(fs::read_to_string(&file_a).unwrap(), "A updated");
    assert_eq!(fs::read_to_string(&file_b).unwrap(), r#"{"b": 2}"#);
    assert_eq!(fs::read_to_string(&file_c).unwrap(), "c = 3");
    assert!(!temp_dir.join(".tx_journal").exists());

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_rollback_on_explicit_drop() {
    let temp_dir = std::env::temp_dir().join(format!("tx_test_drop_{}", std::process::id()));
    fs::create_dir_all(&temp_dir).unwrap();

    let target = temp_dir.join("preserve.txt");
    fs::write(&target, "do not lose me").unwrap();

    {
        let mut tx = FileTransaction::begin(&temp_dir).unwrap();
        tx.add_file_bytes(&target, b"new tentative content").unwrap();
        assert!(temp_dir.join("preserve.txt.tmp").exists());
        // commit() を呼ばずにブロックを抜ける（Drop）
    }

    // Drop による自動ロールバック後: 元の内容が保たれ、一時ファイルもジャーナルも消える
    assert_eq!(fs::read_to_string(&target).unwrap(), "do not lose me");
    assert!(!temp_dir.join("preserve.txt.tmp").exists());
    assert!(!temp_dir.join(".tx_journal").exists());

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_crash_recovery_from_journal() {
    let temp_dir = std::env::temp_dir().join(format!("tx_test_crash_{}", std::process::id()));
    fs::create_dir_all(&temp_dir).unwrap();

    let file_a = temp_dir.join("A.txt");
    let file_b = temp_dir.join("B.json");

    // クラッシュ前の状態を再現:
    // A は .bak に退避された状態で、target には未確定の一時ファイルまたは置換済みファイルがある
    // B はまだ .tmp だけがある状態
    let bak_a = temp_dir.join("A.txt.bak");
    let tmp_a = temp_dir.join("A.txt.tmp");
    fs::write(&bak_a, "Original A content").unwrap();
    fs::write(&file_a, "Corrupted/Half-written A").unwrap();
    fs::write(&tmp_a, "Temporary A").unwrap();

    let tmp_b = temp_dir.join("B.json.tmp");
    fs::write(&file_b, "Original B content").unwrap();
    fs::write(&tmp_b, "Temporary B").unwrap();

    // クラッシュしたプロセスの残存ジャーナルを作成
    let journal_path = temp_dir.join(".tx_journal");
    let journal_content = format!(
        "{}\t{}\t{}\n{}\t{}\t{}\n",
        file_a.to_string_lossy(),
        bak_a.to_string_lossy(),
        tmp_a.to_string_lossy(),
        file_b.to_string_lossy(),
        temp_dir.join("B.json.bak").to_string_lossy(),
        tmp_b.to_string_lossy(),
    );
    fs::write(&journal_path, journal_content).unwrap();

    assert!(journal_path.exists());
    assert!(bak_a.exists());
    assert!(tmp_a.exists());
    assert!(tmp_b.exists());

    // 次回起動時のリカバリーを実行
    FileTransaction::recover(&temp_dir).unwrap();

    // 検証:
    // A.txt はバックアップから原状回復している
    assert_eq!(fs::read_to_string(&file_a).unwrap(), "Original A content");
    // B.json は元の内容が維持されている
    assert_eq!(fs::read_to_string(&file_b).unwrap(), "Original B content");
    // .tmp, .bak, .tx_journal の残骸ゴミはすべて綺麗に消去されている
    assert!(!bak_a.exists());
    assert!(!tmp_a.exists());
    assert!(!tmp_b.exists());
    assert!(!journal_path.exists());

    let _ = fs::remove_dir_all(&temp_dir);
}
