use std::path::Path;
use std::time::{Duration, Instant};
use planetext_document::Application;

#[test]
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

