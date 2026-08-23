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
use crate::ipc;

/// 一度に取り寄せる行数。見えている窓と少しの余白が 1 回で届く程度。
const CHUNK_LINES: usize = 20_000;

enum Task {
    Fetch(Range<usize>),
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
}

pub(super) fn install(shell: Shell) {
    SHELL.set(Some(shell));
    editor::set_on_missing(Rc::new(move |pane, range| fetch(shell, pane, range)));
    editor::set_on_far_copy(Rc::new(move |pane, copy| {
        if let Some(tab) = shell.tab_of(pane) {
            enqueue(tab, Task::Copy(copy));
        }
    }));
}

/// 画面に入ったのにまだ無い行を取り寄せる。並んでいる取り寄せがあれば合流する。
fn fetch(shell: Shell, editor_pane: usize, range: Range<usize>) {
    let Some(tab) = shell.tab_of(editor_pane) else {
        return;
    };
    let id = tab.id.get_untracked();
    let merged = QUEUES.with(|queues| {
        let mut queues = queues.borrow_mut();
        let queue = queues.entry(id).or_default();
        for (_, task) in queue.iter_mut() {
            if let Task::Fetch(current) = task {
                *current = current.start.min(range.start)..current.end.max(range.end);
                return true;
            }
        }
        false
    });
    if !merged {
        enqueue(tab, Task::Fetch(range));
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

/// 次を検索。手元に全部ある文書はその場で、そうでなければ本体の走査で。
pub(super) fn find(shell: Shell, query: String, options: editor::SearchOptions, file_size: Option<usize>) {
    if editor::fully_resident() {
        editor::find_next(&query, options, file_size);
        return;
    }
    enqueue(
        shell.tab_untracked(),
        Task::Find {
            query,
            options,
            file_size,
        },
    );
}

fn enqueue(tab: Tab, task: Task) {
    let id = tab.id.get_untracked();
    QUEUES.with(|queues| {
        queues
            .borrow_mut()
            .entry(id)
            .or_default()
            .push_back((tab, task))
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
        if !execute(tab, task).await {
            return;
        }
    }
}

/// 1 つの仕事。文書がもう手放されていれば列ごと終わる。
async fn execute(tab: Tab, task: Task) -> bool {
    let Some(shell) = SHELL.get() else {
        return false;
    };
    let Some(handle) = handle_of(tab).await else {
        return false;
    };
    match task {
        Task::Fetch(range) => {
            let count = range.len().min(CHUNK_LINES);
            let Ok(lines) = ipc::read_lines(handle, range.start, count).await else {
                return false;
            };
            if !lines.is_empty() {
                shell.feed(tab, range.start, &lines);
            }
            let rest = range.start + lines.len()..range.end;
            if !rest.is_empty() && !lines.is_empty() {
                enqueue(tab, Task::Fetch(rest));
            }
        }
        Task::Edits(batch) => {
            for edit in &batch.edits {
                if ipc::replace_lines(
                    handle,
                    edit.from,
                    edit.to,
                    &edit.lines,
                    batch.group,
                    &batch.before,
                    &batch.after,
                )
                .await
                .is_err()
                {
                    return false;
                }
            }
        }
        Task::Undo { redo } => {
            if let Some(restored) = ipc::undo_lines(handle, redo).await {
                shell.apply_restored(
                    tab,
                    &restored.state,
                    restored.touched_from,
                    restored.line_count,
                );
                shell.mark_dirty_tab(tab);
            }
        }
        Task::Save { path } => match ipc::save_document(handle, &path).await {
            Ok(()) => {
                tab.path.set(Some(path));
                shell.status.set("保存しました".into());
                shell.mark_clean_tab(tab);
            }
            Err(error) => shell.status.set(error),
        },
        Task::Draft => {
            let id = tab.id.get_untracked();
            let path = tab.path.get_untracked();
            ipc::save_draft(handle, id, path.as_deref()).await;
        }
        Task::Copy(copy) => match assemble_copy(handle, copy).await {
            Ok(()) => shell.status.set("コピーしました".into()),
            Err(error) => shell.status.set(error),
        },
        Task::Find {
            query,
            options,
            file_size,
        } => match find_far(shell, tab, handle, &query, options, file_size).await {
            Ok(_) => {}
            Err(error) => shell.status.set(error),
        },
    }
    true
}

/// 1 回の走査で本体から取り寄せる行数。一致が見つかればもっと早く返る。
const SCAN_LINES: usize = 200_000;

/// 文書の本体を走査して次の一致へ跳ぶ。素の行の一致は本体が見つけ、記法を
/// 含む行だけ取り寄せて手元の構造検索で調べる。端まで行ったら先頭へ回る。
async fn find_far(
    shell: Shell,
    tab: Tab,
    handle: u64,
    query: &str,
    options: editor::SearchOptions,
    file_size: Option<usize>,
) -> Result<bool, String> {
    use crate::format::document;
    use crate::structure::text::Pos;
    let Some(pane) = shell.pane_showing(tab).map(|pane| pane.editor_pane()) else {
        return Ok(false);
    };
    let Some((after, line_count)) = editor::far_search_start() else {
        return Ok(false);
    };
    let start_line = after.0.line;
    // 一巡り: 出発点の行から末尾まで、その後は先頭から出発点の行まで。
    let passes: [(usize, usize, Option<&editor::search::Key>); 2] = [
        (start_line, line_count, Some(&after)),
        (0, (start_line + 1).min(line_count), None),
    ];
    for (mut from, end, filter) in passes {
        while from < end {
            let page = ipc::search_lines(
                handle,
                query,
                options.regex,
                options.case_sensitive,
                document::NOTATION_MARK,
                from,
                (end - from).min(SCAN_LINES),
            )
            .await?;
            for hit in &page.hits {
                if hit.notation {
                    let lines = ipc::read_lines(handle, hit.line, 1).await?;
                    shell.feed(tab, hit.line, &lines);
                    if editor::find_far_in_line(pane, hit.line, query, options, file_size, filter)
                    {
                        return Ok(true);
                    }
                } else {
                    let key = (Pos::new(hit.line, hit.start), None);
                    if filter.is_none_or(|after| &key >= after)
                        && editor::apply_far_match(pane, hit.line, hit.start, hit.end)
                    {
                        return Ok(true);
                    }
                }
            }
            if page.scanned_to <= from {
                break;
            }
            from = page.scanned_to;
        }
    }
    Ok(false)
}

/// まだ届いていない行を含む選択のコピー。記法の解釈を要する行だけを取り寄せて
/// 読み下し、組み立てとクリップボードへの書き込みは本体が行う。
async fn assemble_copy(handle: u64, copy: editor::FarCopy) -> Result<(), String> {
    use crate::format::document;
    use crate::structure::plain;
    use crate::structure::text::SourceLine;
    let mut overrides = copy.overrides;
    let notation =
        ipc::lines_containing(handle, copy.from_line, copy.to_line, document::NOTATION_MARK)
            .await?;
    for line in notation {
        if (line == copy.from_line && copy.first.is_some())
            || (line == copy.to_line && copy.last.is_some())
            || overrides.iter().any(|(l, _)| *l == line)
        {
            continue;
        }
        let text = ipc::read_lines(handle, line, 1).await?;
        let Some(text) = text.first() else { continue };
        let plain = match document::read_line(text) {
            SourceLine::Parsed(row) => plain::row(&row),
            SourceLine::Plain(text) => text,
        };
        overrides.push((line, plain));
    }
    ipc::copy_range(
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
