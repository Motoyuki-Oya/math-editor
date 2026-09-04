//! 文書の本体（ネイティブ側ストア）との同期。行の取り寄せ、編集の引き渡し、
//! 元に戻す・やり直す、保存、下書きが、タブごとに 1 本の列に並んで順に走る。
//!
//! 順序がすべてを守る: 編集より前に並んだ取り寄せは編集前の行番号で、後に
//! 並んだものは編集後の行番号で、本体と手元の両方が同じ順で進むから食い違わない。

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::ops::Range;
use std::rc::Rc;

use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::JsCast;

use super::shell::{Shell, Tab};
use crate::editor;
use crate::framework;

/// 一度に取り寄せる行数。見えている窓と少しの余白が 1 回で届く程度。
const CHUNK_LINES: usize = 20_000;
const TAIL_LINES: usize = 200;

enum Task {
    FetchTail {
        pane: usize,
    },
    Edits(editor::FlushBatch),
    Undo {
        redo: bool,
    },
    Save {
        path: String,
    },
    Draft,
    Copy(editor::FarCopy),
    Find {
        pane: usize,
        query: String,
        options: editor::SearchOptions,
        file_size: Option<usize>,
    },
}

thread_local! {
    static SHELL: Cell<Option<Shell>> = const { Cell::new(None) };
    /// タブごとの、並んでいる仕事。
    static QUEUES: RefCell<HashMap<usize, VecDeque<(Tab, Task)>>> = RefCell::new(HashMap::new());
    /// 列が走っているタブ。二重に走らせない。
    static BUSY: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
    /// 取り寄せ中（行描画）の範囲。重い検索タスクに巻き込まれず最優先で描画する。
    static FETCH_RANGES: RefCell<HashMap<usize, Range<usize>>> = RefCell::new(HashMap::new());
    /// 取り寄せが走っているタブ。
    static FETCH_BUSY: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

pub(super) fn install(shell: Shell) {
    SHELL.set(Some(shell));
    editor::set_on_missing(Rc::new(move |pane, range| fetch(shell, pane, range)));
    editor::set_on_tail(Rc::new(move |pane| {
        if let Some(tab) = shell.tab_of(pane) {
            enqueue(tab, Task::FetchTail { pane });
        }
    }));
    editor::set_on_far_copy(Rc::new(move |pane, copy| {
        if let Some(tab) = shell.tab_of(pane) {
            // 大きな選択の組み立てとクリップボードへの書き込みは時間がかかる。
            // 固まったと思われないように、始めたことを見せる。
            shell.status.set("コピーしています…".into());
            enqueue(tab, Task::Copy(copy));
        }
    }));
}

/// 画面に入ったのにまだ無い行を取り寄せる。重い検索タスクとは独立して最優先で取得し、白飛びを防ぐ。
fn fetch(shell: Shell, editor_pane: usize, range: Range<usize>) {
    if range.is_empty() {
        return;
    }
    let Some(tab) = shell.tab_of(editor_pane) else {
        return;
    };
    let id = tab.id.get_untracked();
    FETCH_RANGES.with(|ranges| {
        let mut ranges = ranges.borrow_mut();
        let current = ranges.entry(id).or_insert_with(|| range.clone());
        if range.start <= current.end && current.start <= range.end {
            *current = current.start.min(range.start)..current.end.max(range.end);
        } else {
            *current = range;
        }
    });
    let busy = FETCH_BUSY.with(|busy| busy.borrow().contains(&id));
    if busy {
        return;
    }
    FETCH_BUSY.with(|busy| busy.borrow_mut().push(id));
    spawn_local(async move {
        run_fetch(shell, tab).await;
        FETCH_BUSY.with(|busy| busy.borrow_mut().retain(|other| *other != id));
    });
}

async fn run_fetch(shell: Shell, tab: Tab) {
    let id = tab.id.get_untracked();
    loop {
        let range = FETCH_RANGES.with(|ranges| ranges.borrow_mut().remove(&id));
        let Some(range) = range else {
            break;
        };
        if range.is_empty() {
            continue;
        }
        let Some(handle) = handle_of(tab).await else {
            break;
        };
        let count = range.len().min(CHUNK_LINES);
        let Ok(read_res) = framework::read_lines(handle, range.start, count).await else {
            break;
        };
        if !read_res.lines.is_empty() {
            shell.feed(tab, read_res.from, &read_res.lines);
        }
        let rest = read_res.from + read_res.lines.len()..range.end;
        if !rest.is_empty() && !read_res.lines.is_empty() {
            FETCH_RANGES.with(|ranges| {
                let mut ranges = ranges.borrow_mut();
                let current = ranges.entry(id).or_insert_with(|| rest.clone());
                if rest.start <= current.end && current.start <= rest.end {
                    *current = current.start.min(rest.start)..current.end.max(rest.end);
                } else {
                    *current = rest;
                }
            });
        }
    }
}

/// たまった編集を本体へ。文書が変わるたびに呼ばれる。
pub(super) fn flush(shell: Shell, editor_pane: usize) {
    let Some(batch) = editor::take_flush(editor_pane) else {
        return;
    };
    let Some(tab) = shell.tab_of(editor_pane) else {
        return;
    };
    enqueue(tab, Task::Edits(batch));
}

pub(super) fn undo(shell: Shell, redo: bool) {
    let tab = shell.tab_untracked();
    enqueue(tab, Task::Undo { redo });
}

pub(super) fn save(tab: Tab, path: String) {
    enqueue(tab, Task::Save { path });
}

pub(super) fn draft(tab: Tab) {
    enqueue(tab, Task::Draft);
}

/// 次を検索。手元に届いている行にあればその場で即座にジャンプ、そうでなければ本体の走査で。
pub(super) fn find(
    shell: Shell,
    pane: usize,
    query: String,
    options: editor::SearchOptions,
    file_size: Option<usize>,
) {
    if editor::find_next_resident(&query, options, file_size) {
        if let Some(pane_obj) = shell.pane_for_editor(pane) {
            let total = pane_obj.search_status.get_untracked().1;
            let num = editor::current_match_number(&query, options, total.unwrap_or(0));
            pane_obj.search_status.set((num, total));
        }
        return;
    }
    if editor::fully_resident() {
        if editor::find_next(&query, options, file_size) {
            if let Some(pane_obj) = shell.pane_for_editor(pane) {
                let total = pane_obj.search_status.get_untracked().1;
                let num = editor::current_match_number(&query, options, total.unwrap_or(0));
                pane_obj.search_status.set((num, total));
            }
        }
        return;
    }
    let Some(tab) = shell.tab_of(pane) else {
        return;
    };
    drop_pending_find(tab);
    shell.status.set("検索しています…".into());
    enqueue(
        tab,
        Task::Find {
            pane,
            query,
            options,
            file_size,
        },
    );
}

/// 前を検索。手元に届いている行にあればその場で即座にジャンプ、そうでなければ全体から。
pub(super) fn find_previous(
    shell: Shell,
    pane: usize,
    query: String,
    options: editor::SearchOptions,
    file_size: Option<usize>,
) {
    if editor::find_previous_resident(&query, options, file_size) {
        if let Some(pane_obj) = shell.pane_for_editor(pane) {
            let total = pane_obj.search_status.get_untracked().1;
            let num = editor::current_match_number(&query, options, total.unwrap_or(0));
            pane_obj.search_status.set((num, total));
        }
        return;
    }
    if editor::find_previous(&query, options, file_size) {
        if let Some(pane_obj) = shell.pane_for_editor(pane) {
            let total = pane_obj.search_status.get_untracked().1;
            let num = editor::current_match_number(&query, options, total.unwrap_or(0));
            pane_obj.search_status.set((num, total));
        }
    }
}

fn drop_pending_find(tab: Tab) {
    let id = tab.id.get_untracked();
    QUEUES.with(|queues| {
        let mut queues = queues.borrow_mut();
        if let Some(queue) = queues.get_mut(&id) {
            queue.retain(|(_, task)| !matches!(task, Task::Find { .. }));
        }
    });
}

fn enqueue(tab: Tab, task: Task) {
    let id = tab.id.get_untracked();
    QUEUES.with(|queues| {
        let mut queues = queues.borrow_mut();
        let queue = queues.entry(id).or_default();
        queue.push_back((tab, task));
    });
    let busy = BUSY.with(|busy| busy.borrow().contains(&id));
    if busy {
        return;
    }
    BUSY.with(|busy| busy.borrow_mut().push(id));
    spawn_local(async move {
        run(id).await;
        BUSY.with(|busy| busy.borrow_mut().retain(|other| *other != id));
    });
}

async fn run(id: usize) {
    let Some(shell) = SHELL.get() else {
        return;
    };
    loop {
        let next = QUEUES.with(|queues| {
            queues
                .borrow_mut()
                .get_mut(&id)
                .and_then(VecDeque::pop_front)
        });
        let Some((tab, task)) = next else {
            QUEUES.with(|queues| {
                queues.borrow_mut().remove(&id);
            });
            return;
        };
        if !execute(shell, tab, task).await {
            return;
        }
    }
}

/// 1 つの仕事をこなす。失敗したら false を返し、列を止める。
async fn execute(shell: Shell, tab: Tab, task: Task) -> bool {
    let Some(handle) = handle_of(tab).await else {
        return false;
    };
    match task {
        Task::FetchTail { pane } => match framework::read_tail(handle, TAIL_LINES).await {
            Ok(lines) => {
                editor::show_tail(pane, &lines);
                shell.status.set("末尾を表示しました（行数確認中）".into());
            }
            Err(error) => shell.status.set(error),
        },
        Task::Edits(batch) => {
            for edit in &batch.edits {
                if framework::replace_lines(
                    handle,
                    edit.from,
                    edit.to,
                    &edit.lines,
                    batch.group,
                    &batch.before,
                    &batch.after,
                    None,
                )
                .await
                .is_err()
                {
                    return false;
                }
            }
        }
        Task::Undo { redo } => {
            if let Some(restored) = framework::undo_lines(handle, redo).await {
                shell.apply_restored(
                    tab,
                    &restored.state,
                    restored.touched_from,
                    restored.line_count,
                );
                let doc_id = tab.id.get_untracked();
                let doc_ref = editor::get_or_create_doc(doc_id);
                doc_ref
                    .borrow_mut()
                    .set_modified_lines(restored.modified_lines);
                if restored.clean {
                    shell.mark_clean_tab(tab);
                } else {
                    shell.mark_dirty_tab(tab);
                }
                let focused_pane = shell.pane_showing(tab).map(|p| p.editor_pane());
                editor::redraw_doc(doc_id, focused_pane);
            }
        }
        Task::Save { path } => match framework::save_document(handle, &path).await {
            Ok(()) => {
                editor::set_doc_path(tab.id.get_untracked(), Some(path.clone()));
                tab.path.set(Some(path));
                tab.untitled_num.set(None);
                shell.status.set("保存しました".into());
                shell.mark_clean_tab(tab);
            }
            Err(error) => shell.status.set(error),
        },
        Task::Draft => {
            let id = tab.id.get_untracked();
            let path = tab.path.get_untracked();
            framework::save_draft(handle, id, path.as_deref()).await;
        }
        Task::Copy(copy) => match assemble_copy(handle, copy).await {
            Ok(()) => shell.status.set("コピーしました".into()),
            Err(error) => shell.status.set(error),
        },
        Task::Find {
            pane,
            query,
            options,
            file_size,
        } => {
            let result = find_far(shell, tab, pane, handle, &query, options, file_size).await;
            match result {
                Ok(Some(true)) => shell.status.set("見つかりました".into()),
                Ok(Some(false)) => shell.status.set("見つかりませんでした".into()),
                Ok(None) => {}
                Err(error) => shell.status.set(error),
            }
        }
    }
    true
}

/// 文書の本体を走査して次の一致へ跳ぶ。空のページはnative内で読み進め、
/// 記法を含む候補だけ必要に応じて手元で確認する。
/// `None` は新しい検索や編集によるキャンセル。
async fn find_far(
    shell: Shell,
    tab: Tab,
    pane: usize,
    handle: u64,
    query: &str,
    options: editor::SearchOptions,
    file_size: Option<usize>,
) -> Result<Option<bool>, String> {
    use crate::format::document;
    use crate::structure::text::Pos;
    let Some((after, line_count)) = editor::far_search_start() else {
        return Ok(Some(false));
    };
    let start_line = after.0.line;
    // 一巡り: 出発点の行から末尾まで、その後は先頭から出発点の行まで。
    let passes: [(usize, usize, Option<&editor::search::Key>); 2] = [
        (start_line, line_count, Some(&after)),
        (0, (start_line + 1).min(line_count), None),
    ];
    for (mut from, end, filter) in passes {
        let mut after_col = filter.map(|after| after.0.col);
        while from < end {
            let page = framework::search_document(
                handle,
                query,
                options.regex,
                options.case_sensitive,
                document::NOTATION_MARK,
                from,
                end,
                after_col,
            )
            .await?;
            if page.cancelled {
                return Ok(None);
            }
            for hit in &page.hits {
                if hit.notation {
                    let lines = framework::read_lines(handle, hit.line, 1).await?;
                    shell.feed(tab, hit.line, &lines);
                    if editor::find_far_in_line(pane, hit.line, query, options, file_size, filter) {
                        if let Some(pane_obj) = shell.pane_for_editor(pane) {
                            if let Some(cur) = page.current_index {
                                pane_obj.search_status.set((cur, page.total_matches));
                            }
                        }
                        return Ok(Some(true));
                    }
                } else {
                    let key = (Pos::new(hit.line, hit.start), None);
                    if filter.is_none_or(|after| &key >= after)
                        && editor::apply_far_match(pane, hit.line, hit.start, hit.end)
                    {
                        if let Some(pane_obj) = shell.pane_for_editor(pane) {
                            if let Some(cur) = page.current_index {
                                pane_obj.search_status.set((cur, page.total_matches));
                            }
                        }
                        return Ok(Some(true));
                    }
                }
            }
            if page.scanned_to <= from {
                break;
            }
            from = page.scanned_to;
            after_col = None;
        }
    }
    Ok(Some(false))
}

/// まだ届いていない行を含む選択のコピー。記法の解釈を要する行だけを取り寄せて
/// 読み下し、組み立てとクリップボードへの書き込みは本体が行う。
async fn assemble_copy(handle: u64, copy: editor::FarCopy) -> Result<(), String> {
    use crate::format::document;
    use crate::structure::plain;
    use crate::structure::text::SourceLine;
    let mut overrides = copy.overrides;
    let notation = framework::lines_containing(
        handle,
        copy.from_line,
        copy.to_line,
        document::NOTATION_MARK,
    )
    .await?;
    for line in notation {
        if (line == copy.from_line && copy.first.is_some())
            || (line == copy.to_line && copy.last.is_some())
            || overrides.iter().any(|(l, _)| *l == line)
        {
            continue;
        }
        let text = framework::read_lines(handle, line, 1).await?;
        let Some(text) = text.first() else { continue };
        let plain = match document::read_line(text) {
            SourceLine::Parsed(row) => plain::row(&row),
            SourceLine::Plain(text) => text,
        };
        overrides.push((line, plain));
    }
    framework::copy_range(
        handle,
        copy.from_line,
        copy.first.as_deref(),
        copy.to_line,
        copy.last.as_deref(),
        &overrides,
    )
    .await
}

/// タブの文書の取っ手。新しいタブでは作成が届くまで少し待つ。
async fn handle_of(tab: Tab) -> Option<u64> {
    for _ in 0..200 {
        if let Some(handle) = tab.doc.get_untracked() {
            return Some(handle);
        }
        tick(10).await;
    }
    None
}

/// ブラウザーのタイマーで 1 回眠る。列が取っ手の到着を待つのに使う。
async fn tick(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        if let Some(window) = web_sys::window() {
            window
                .set_timeout_with_callback_and_timeout_and_arguments_0(resolve.unchecked_ref(), ms)
                .ok();
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}
