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

    // UI検索パイプラインと同等範囲での計測

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
        true,
    ).expect("prepare_search failed");

    let page = job.run().expect("job.run failed");
    let duration_cs = t_search_start.elapsed();
    let t_pipe_first = Instant::now();
    let hit_line = page.hits[0].line;
    let _read = app.read_lines(handle, hit_line, 1).expect("read_lines failed");
    let duration_pipe_first = duration_cs + t_pipe_first.elapsed();
    println!("[BENCH 50MB] Case-sensitive search took {:?}, pipeline (search + read_lines): {:?}, hits found: {}",
        duration_cs, duration_pipe_first, page.hits.len());
    assert_eq!(page.hits.len(), 1, "末尾のヒットが検出されること");
    assert_eq!(page.hits[0].line, target_line);

    // 2. 2回目（キャッシュ利用）のUI検索パイプライン
    let t_cached_start = Instant::now();
    let job_cached = app.prepare_search(
        handle,
        "cccquick".to_string(),
        false,
        true,
        '\0',
        0,
        scanned_lines,
        None,
        true,
    ).expect("prepare_search failed");
    let page_cached = job_cached.run().expect("run failed");
    let hit_line_cached = page_cached.hits[0].line;
    let _read_cached = app.read_lines(handle, hit_line_cached, 1).expect("read_lines failed");
    let cached_duration = t_cached_start.elapsed();
    println!("[BENCH 50MB] 2nd search pipeline (cached search + read_lines): {:?}, hits found: {}",
        cached_duration, page_cached.hits.len());
    assert_eq!(page_cached.hits.len(), 1);

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
        true,
    ).expect("prepare_search failed");
    let page_ci = job_ci.run().expect("job.run failed");
    let hit_line_ci = page_ci.hits[0].line;
    let _read_ci = app.read_lines(handle, hit_line_ci, 1).expect("read_lines failed");
    let ci_duration = t_ci_start.elapsed();
    println!("[BENCH 50MB] Case-insensitive pipeline (search + read_lines): {:?}, hits found: {}",
        ci_duration, page_ci.hits.len());
    assert_eq!(page_ci.hits.len(), 1);

    // 4. 「前へ」（逆方向検索: forward = false）UI検索パイプライン
    let t_prev_start = Instant::now();
    let job_prev = app.prepare_search(
        handle,
        "cccquick".to_string(),
        false,
        true,
        '\0',
        scanned_lines,
        scanned_lines,
        None,
        false, // forward: false (前を検索)
    ).expect("prepare_search failed");
    let page_prev = job_prev.run().expect("job.run failed");
    assert!(!page_prev.hits.is_empty(), "前へのヒットが見つかること");
    let hit_prev = &page_prev.hits[0];
    let _read_prev = app.read_lines(handle, hit_prev.line, 1).expect("read_lines failed");
    let prev_duration = t_prev_start.elapsed();
    println!("[BENCH 50MB] Previous search pipeline (forward=false search + read_lines): {:?}, hit line: {}, current_index: {:?}",
        prev_duration, hit_prev.line, page_prev.current_index);
    // 5. 2回目以降の「前へ」: 直前のヒット位置 (target_line, hit_prev.start) から前を検索した場合、
    // 自分自身に留まらず、正しく直前（先頭ラップアラウンドなど）へ進むこと
    let job_prev_again = app.prepare_search(
        handle,
        "cccquick".to_string(),
        false,
        true,
        '\0',
        hit_prev.line,
        scanned_lines,
        Some(hit_prev.start),
        false, // forward: false (前へ)
    ).expect("prepare_search failed");
    let page_prev_again = job_prev_again.run().expect("job.run failed");
    assert!(!page_prev_again.hits.is_empty(), "前へ再検索でもヒットすること");
    // 1件しか存在しないファイルなので、自分自身（start位置）より前にはないため、末尾（ラップアラウンド）へ回る
    assert_eq!(page_prev_again.hits[0].line, target_line);
    assert_eq!(page_prev_again.current_index, Some(1));

    app.close_document(handle);
    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn bench_parallel_comparison() {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_dir = std::env::temp_dir().join(format!("bench_parallel_{timestamp}"));
    std::fs::create_dir_all(&temp_dir).unwrap();
    let file_path = temp_dir.join("test-50mb-par.txt");

    let total_lines = 1_000_000;
    {
        let file = File::create(&file_path).unwrap();
        let mut writer = BufWriter::with_capacity(1024 * 1024, file);
        for i in 0..total_lines {
            if i == 999_990 {
                writer.write_all(b"marker line at end target_kw_th1 target_kw_th2 target_kw_th4 target_kw_th8\n").unwrap();
            } else if i == 999_991 {
                writer.write_all(b"marker line regex regex_pat_th1_end regex_pat_th2_end regex_pat_th4_end regex_pat_th8_end\n").unwrap();
            } else {
                writer.write_all(b"regular benchmark content line for fifty megabytes text test\n").unwrap();
            }
        }
        writer.flush().unwrap();
    }

    let file_size = std::fs::metadata(&file_path).unwrap().len();
    println!("\n=======================================================");
    println!("[PARALLEL BENCHMARK] 50MB ({file_size} bytes, {total_lines} lines)");
    println!("=======================================================");

    let app = Application::default();
    let opened = app.open_document(file_path.to_str().unwrap().to_string()).unwrap();
    let handle = opened.handle;

    let finish_job = app.finish_document(handle).unwrap();
    let scanned_lines = loop {
        if let Some(count) = finish_job.poll().unwrap() {
            break count;
        }
        std::thread::sleep(Duration::from_millis(10));
    };

    let thread_counts = [1, 2, 4, 8];

    // 1. リテラル検索（末尾ヒット・全文走査）
    println!("\n--- 1. Literal Search (Case-Sensitive, Full File Scan) ---");
    for &th in &thread_counts {
        std::env::set_var("PLANETEXT_SEARCH_THREADS", th.to_string());
        let query = format!("target_kw_th{th}");
        let t0 = Instant::now();
        let job = app.prepare_search(
            handle,
            query,
            false,
            true,
            '\0',
            0,
            scanned_lines,
            None,
            true,
        ).unwrap();
        let page = job.run().unwrap();
        let elapsed = t0.elapsed();
        println!("Threads = {:>2} | Elapsed = {:>8.2?} | Hits = {}", th, elapsed, page.hits.len());
        assert_eq!(page.hits.len(), 1);
    }

    // 2. リテラル検索（大文字小文字無視・全文走査）
    println!("\n--- 2. Literal Search (Case-Insensitive, Full File Scan) ---");
    for &th in &thread_counts {
        std::env::set_var("PLANETEXT_SEARCH_THREADS", th.to_string());
        let query = format!("TARGET_KW_TH{th}");
        let t0 = Instant::now();
        let job = app.prepare_search(
            handle,
            query,
            false,
            false,
            '\0',
            0,
            scanned_lines,
            None,
            true,
        ).unwrap();
        let page = job.run().unwrap();
        let elapsed = t0.elapsed();
        println!("Threads = {:>2} | Elapsed = {:>8.2?} | Hits = {}", th, elapsed, page.hits.len());
        assert_eq!(page.hits.len(), 1);
    }

    // 3. 正規表現検索（全文走査）
    println!("\n--- 3. Regex Search (Case-Sensitive, Full File Scan) ---");
    for &th in &thread_counts {
        std::env::set_var("PLANETEXT_SEARCH_THREADS", th.to_string());
        let query = format!(r"regex_pat_th{th}_[a-z]+");
        let t0 = Instant::now();
        let job = app.prepare_search(
            handle,
            query,
            true,
            true,
            '\0',
            0,
            scanned_lines,
            None,
            true,
        ).unwrap();
        let page = job.run().unwrap();
        let elapsed = t0.elapsed();
        println!("Threads = {:>2} | Elapsed = {:>8.2?} | Hits = {}", th, elapsed, page.hits.len());
        assert_eq!(page.hits.len(), 1);
    }

    // 4. 正規表現検索（大文字小文字無視・全文走査）
    println!("\n--- 4. Regex Search (Case-Insensitive, Full File Scan) ---");
    for &th in &thread_counts {
        std::env::set_var("PLANETEXT_SEARCH_THREADS", th.to_string());
        let query = format!(r"REGEX_PAT_TH{th}_[a-z]+");
        let t0 = Instant::now();
        let job = app.prepare_search(
            handle,
            query,
            true,
            false,
            '\0',
            0,
            scanned_lines,
            None,
            true,
        ).unwrap();
        let page = job.run().unwrap();
        let elapsed = t0.elapsed();
        println!("Threads = {:>2} | Elapsed = {:>8.2?} | Hits = {}", th, elapsed, page.hits.len());
        assert_eq!(page.hits.len(), 1);
    }

    // 5. ミスマッチ走査（完全なワーストケース：0件ヒットの全バイト精査）
    println!("\n--- 5. Worst-Case Miss Scan (0 hits, 100% Haystack Traversal) ---");
    for &th in &thread_counts {
        std::env::set_var("PLANETEXT_SEARCH_THREADS", th.to_string());
        let query = format!("absolutely_nonexistent_token_threads_{th}");
        let t0 = Instant::now();
        let job = app.prepare_search(
            handle,
            query,
            false,
            true,
            '\0',
            0,
            scanned_lines,
            None,
            true,
        ).unwrap();
        let page = job.run().unwrap();
        let elapsed = t0.elapsed();
        println!("Threads = {:>2} | Elapsed = {:>8.2?} | Hits = {}", th, elapsed, page.hits.len());
        assert_eq!(page.hits.len(), 0);
    }

    app.close_document(handle);
    let _ = std::fs::remove_dir_all(&temp_dir);
}
