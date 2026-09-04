use std::fs::File;
use std::io::{BufWriter, Write};
use std::time::{Duration, Instant};
use planetext_document::Application;

#[test]
fn bench_search_50mb() {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_dir = std::env::temp_dir().join(format!("bench_50mb_{timestamp}"));
    std::fs::create_dir_all(&temp_dir).unwrap();
    let file_path = temp_dir.join("test-50mb.txt");

    // 1. 50MB（約100万行）のファイルを生成。
    // 末尾付近（999,998 行目）に "cccquick" を仕込む。
    let target_line = 999_998;
    let total_lines = 1_000_000;
    {
        let file = File::create(&file_path).unwrap();
        let mut writer = BufWriter::with_capacity(1024 * 1024, file);
        for i in 0..total_lines {
            if i == target_line {
                writer.write_all(b"line target with key cccquick embedded here\n").unwrap();
            } else if i % 100_000 == 0 {
                let milestone = format!("milestone line {i} in 50mb benchmark\n");
                writer.write_all(milestone.as_bytes()).unwrap();
            } else {
                writer.write_all(b"regular benchmark content line for fifty megabytes text test\n").unwrap();
            }
        }
        writer.flush().unwrap();
    }

    let file_size = std::fs::metadata(&file_path).unwrap().len();
    println!("\n[BENCH 50MB] Generated {file_size} bytes ({total_lines} lines).");

    let app = Application::default();
    let path_str = file_path.to_str().unwrap().to_string();

    let t_open_start = Instant::now();
    let opened = app.open_document(path_str).expect("Failed to open");
    let handle = opened.handle;
    println!("[BENCH 50MB] Document opened in {:?}", t_open_start.elapsed());

    // 背景走査の完了を待機
    let t_scan_start = Instant::now();
    let finish_job = app.finish_document(handle).expect("finish_document failed");
    let scanned_lines = loop {
        if let Some(count) = finish_job.poll().expect("poll failed") {
            break count;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    println!("[BENCH 50MB] Scan completed in {:?}, total lines: {scanned_lines}", t_scan_start.elapsed());
    assert!(scanned_lines >= total_lines);

    // インデックス構築完了を待機
    let t_index_start = Instant::now();
    let (indexed, total_b) = loop {
        if let Ok(Some((indexed, total_b))) = app.search_index_progress(handle) {
            if indexed >= total_b && total_b > 0 {
                break (indexed, total_b);
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    println!("[BENCH 50MB] Bigram Index completed in {:?}, blocks: {indexed}/{total_b}", t_index_start.elapsed());

    // 1. Case-sensitive 検索で末尾の cccquick を検索
    let t_search_start = Instant::now();
    let job = app.prepare_search(
        handle,
        "cccquick".to_string(),
        false, // regex
        true,  // case_sensitive
        '\0',
        0,
        scanned_lines,
        None,
    ).expect("prepare_search failed");

    let page = job.run().expect("job.run failed");
    let duration_cs = t_search_start.elapsed();
    println!("[BENCH 50MB] Case-sensitive search took {:?}, hits found: {}", duration_cs, page.hits.len());
    assert_eq!(page.hits.len(), 1, "末尾のヒットが検出されること");
    assert_eq!(page.hits[0].line, target_line);

    // 2. 直前（990,000行目）からのダイレクト検索
    let t_direct_start = Instant::now();
    let job_direct = app.prepare_search(
        handle,
        "cccquick".to_string(),
        false,
        true,
        '\0',
        990_000,
        scanned_lines,
        None,
    ).expect("prepare_search failed");
    let page_direct = job_direct.run().expect("run failed");
    let direct_duration = t_direct_start.elapsed();
    println!("[BENCH 50MB] Direct search took {:?}, hits found: {}", direct_duration, page_direct.hits.len());
    assert_eq!(page_direct.hits.len(), 1);

    // 3. Case-insensitive 検索
    let t_ci_start = Instant::now();
    let job_ci = app.prepare_search(
        handle,
        "CCCQUICK".to_string(),
        false,
        false,
        '\0',
        0,
        scanned_lines,
        None,
    ).expect("prepare_search failed");
    let page_ci = job_ci.run().expect("job.run failed");
    let ci_duration = t_ci_start.elapsed();
    println!("[BENCH 50MB] Case-insensitive search took {:?}, hits found: {}", ci_duration, page_ci.hits.len());
    assert_eq!(page_ci.hits.len(), 1);

    app.close_document(handle);
    let _ = std::fs::remove_dir_all(&temp_dir);
}
