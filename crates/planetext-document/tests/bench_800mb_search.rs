use std::path::Path;
use std::time::{Duration, Instant};
use planetext_document::Application;

#[test]
#[ignore] // 800MB（約6分のインデックス構築）の大規模検証のため任意実行（cargo test --test bench_800mb_search -- --ignored --nocapture）
fn bench_search_800mb() {
    let path = "C:\\workspace\\test-800mb.txt";
    if !Path::new(path).exists() {
        println!("File {path} does not exist, skipping.");
        return;
    }

    let app = Application::default();
    println!("Opening document: {path}...");
    let t_open_start = Instant::now();
    let opened = app.open_document(path.to_string()).expect("Failed to open");
    let handle = opened.handle;
    println!("Document opened in {:?}, handle: {handle}", t_open_start.elapsed());

    // 背景走査の完了を待機
    println!("Waiting for background scan to finish...");
    let t_scan_start = Instant::now();
    let finish_job = app.finish_document(handle).expect("finish_document failed");
    let total_lines = loop {
        if let Some(count) = finish_job.poll().expect("poll failed") {
            break count;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let scan_duration = t_scan_start.elapsed();
    println!("Scan completed in {:?}, total lines: {total_lines}", scan_duration);

    // インデックス構築完了を待機して時間を計測
    println!("Waiting for background Bigram index to finish...");
    let t_index_start = Instant::now();
    let (indexed, total_b) = loop {
        if let Ok(Some((indexed, total_b))) = app.search_index_progress(handle) {
            if indexed >= total_b && total_b > 0 {
                break (indexed, total_b);
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let index_duration = t_index_start.elapsed();
    let total_index_time = t_open_start.elapsed();
    println!("Bigram Index completed in {:?} (from open: {:?}), blocks: {indexed}/{total_b}",
        index_duration, total_index_time);

    // 1. case_sensitive = true で 15,999,998 行目のヒットに到達するまでの検索計測
    println!("\n--- Case Sensitive Search: loop until line 15,999,998 ---");
    let t_search_start = Instant::now();
    let mut total_hits = 0;
    let mut current_from = 0;
    let mut iterations = 0;
    let mut target_hit_found = false;
    let mut target_hit_duration = None;

    while current_from < total_lines {
        iterations += 1;
        let job = app.prepare_search(
            handle,
            "cccquick".to_string(),
            false, // regex
            true,  // case_sensitive
            '\0',
            current_from,
            total_lines,
            None,
            true,
        ).expect("prepare_search failed");

        let page = job.run().expect("job.run failed");
        for hit in &page.hits {
            total_hits += 1;
            if hit.line >= 15_999_990 {
                println!("Found target hit! Line: {}, start: {}, end: {} (at iteration: {}, elapsed: {:?})",
                    hit.line, hit.start, hit.end, iterations, t_search_start.elapsed());
                target_hit_found = true;
                if target_hit_duration.is_none() {
                    target_hit_duration = Some(t_search_start.elapsed());
                }
            }
        }
        if page.scanned_to <= current_from || page.scanned_to >= total_lines {
            break;
        }
        current_from = page.scanned_to;
        if target_hit_found {
            break;
        }
    }
    let duration_cs = target_hit_duration.unwrap_or_else(|| t_search_start.elapsed());
    println!("Case-sensitive reached target in {:?} (iterations: {}, total hits so far: {})",
        duration_cs, iterations, total_hits);

    // 2. 15,000,000行目から直接末尾の15,999,998行目を検索したときの時間
    println!("\n--- Direct Search from line 15,000,000 (Case-sensitive) ---");
    let t_direct_start = Instant::now();
    let job_direct = app.prepare_search(
        handle,
        "cccquick".to_string(),
        false,
        true,
        '\0',
        15_000_000,
        total_lines,
        None,
        true,
    ).expect("prepare_search failed");
    let page_direct = job_direct.run().expect("run failed");
    let direct_duration = t_direct_start.elapsed();
    println!("Direct search took {:?}, hits found: {}", direct_duration, page_direct.hits.len());
    for hit in &page_direct.hits {
        println!("Direct hit at line: {}, start: {}, end: {}", hit.line, hit.start, hit.end);
    }

    // 3. case_sensitive = false での計測
    println!("\n--- Case Insensitive Search: loop until line 15,999,998 ---");
    let t_search_start_ci = Instant::now();
    let mut total_hits_ci = 0;
    let mut current_from_ci = 0;
    let mut iterations_ci = 0;
    let mut target_hit_found_ci = false;
    let mut target_hit_duration_ci = None;

    while current_from_ci < total_lines {
        iterations_ci += 1;
        let job = app.prepare_search(
            handle,
            "cccquick".to_string(),
            false,
            false,
            '\0',
            current_from_ci,
            total_lines,
            None,
            true,
        ).expect("prepare_search failed");

        let page = job.run().expect("job.run failed");
        for hit in &page.hits {
            total_hits_ci += 1;
            if hit.line >= 15_999_990 {
                println!("Found target hit! Line: {}, start: {}, end: {} (at iteration: {}, elapsed: {:?})",
                    hit.line, hit.start, hit.end, iterations_ci, t_search_start_ci.elapsed());
                target_hit_found_ci = true;
                if target_hit_duration_ci.is_none() {
                    target_hit_duration_ci = Some(t_search_start_ci.elapsed());
                }
            }
        }
        if page.scanned_to <= current_from_ci || page.scanned_to >= total_lines {
            break;
        }
        current_from_ci = page.scanned_to;
        if target_hit_found_ci {
            break;
        }
    }
    let duration_ci = target_hit_duration_ci.unwrap_or_else(|| t_search_start_ci.elapsed());
    println!("Case-insensitive reached target in {:?} (iterations: {}, total hits so far: {})",
        duration_ci, iterations_ci, total_hits_ci);

    app.close_document(handle);
}

#[test]
#[ignore]
fn bench_800mb_japanese_and_prev() {
    let path = "C:\\workspace\\test-800mb.txt";
    if !Path::new(path).exists() {
        println!("File {path} does not exist, skipping.");
        return;
    }

    let app = Application::default();
    let opened = app.open_document(path.to_string()).expect("Failed to open");
    let handle = opened.handle;

    let finish_job = app.finish_document(handle).expect("finish_document failed");
    let total_lines = loop {
        if let Some(count) = finish_job.poll().expect("poll failed") {
            break count;
        }
        std::thread::sleep(Duration::from_millis(50));
    };

    // 1. 日本語（非ASCII）検索: case_sensitive = false でも mmap 直接走査で 0.3秒台で完了すること
    let t_ja = Instant::now();
    let job_ja = app.prepare_search(
        handle,
        "テスト文字列".to_string(),
        false, // regex
        false, // case_sensitive: false
        '\0',
        0,
        total_lines,
        None,
        true,
    ).expect("prepare_search failed");
    let page_ja = job_ja.run().expect("run failed");
    let ja_duration = t_ja.elapsed();
    println!("[BENCH 800MB] Japanese search (case_sensitive=false) took {:?}, hits={}", ja_duration, page_ja.hits.len());
    assert!(ja_duration < Duration::from_secs(2), "日本語検索がフォールバックせず高速に完了すること");

    // 2. 「前へ」逆方向検索
    let t_prev = Instant::now();
    let job_prev = app.prepare_search(
        handle,
        "cccquick".to_string(),
        false,
        true,
        '\0',
        total_lines,
        total_lines,
        None,
        false,
    ).expect("prepare_search failed");
    let page_prev = job_prev.run().expect("run failed");
    let prev_duration = t_prev.elapsed();
    println!("[BENCH 800MB] Previous search took {:?}, hits={}", prev_duration, page_prev.hits.len());
    assert!(!page_prev.hits.is_empty());

    // 3. 2回目の「前へ」: 直前のヒット位置から前へ再検索したときに自分自身に留まらないこと
    let hit0 = &page_prev.hits[0];
    let job_prev2 = app.prepare_search(
        handle,
        "cccquick".to_string(),
        false,
        true,
        '\0',
        hit0.line,
        total_lines,
        Some(hit0.start),
        false,
    ).expect("prepare_search failed");
    let page_prev2 = job_prev2.run().expect("run failed");
    assert!(!page_prev2.hits.is_empty());
    let hit1 = &page_prev2.hits[0];
    assert!(hit1.line != hit0.line || hit1.start != hit0.start, "直前の一致自身に留まらず別のヒットへ移動すること");

    app.close_document(handle);
}

