//! 見えている窓の行の取り寄せ。エディターが「この範囲がまだ無い」と知らせて
//! きたら、そのペインの文書からネイティブ側の本体へ取りに行く。
//!
//! 要求はペインごとに 1 つの範囲へまとめ、取り寄せは 1 本ずつ走る。届いた行は
//! [`crate::editor::feed_pane`] が入れて描き直し、まだ足りなければ描き直しが
//! また要求してくる。この往復が、スクロールで先へ行っても見えている場所から
//! 埋まる仕組みそのもの。

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use leptos::task::spawn_local;

use super::shell::Shell;
use crate::editor;
use crate::ipc;

/// 一度に取り寄せる行数。見えている窓と少しの余白が 1 回で届く程度。
const CHUNK_LINES: usize = 20_000;

thread_local! {
    /// ペインごとの、まだ取りに行っていない要求範囲。新しい要求は合流する。
    static PENDING: RefCell<HashMap<usize, Range<usize>>> = RefCell::new(HashMap::new());
    /// 取り寄せが走っているペイン。二重に走らせない。
    static BUSY: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

pub(super) fn install(shell: Shell) {
    editor::set_on_missing(Rc::new(move |pane, range| request(shell, pane, range)));
}

fn request(shell: Shell, editor_pane: usize, range: Range<usize>) {
    PENDING.with(|pending| {
        let mut pending = pending.borrow_mut();
        let merged = match pending.get(&editor_pane) {
            Some(current) => current.start.min(range.start)..current.end.max(range.end),
            None => range,
        };
        pending.insert(editor_pane, merged);
    });
    let busy = BUSY.with(|busy| busy.borrow().contains(&editor_pane));
    if busy {
        return;
    }
    BUSY.with(|busy| busy.borrow_mut().push(editor_pane));
    spawn_local(async move {
        run(shell, editor_pane).await;
        BUSY.with(|busy| busy.borrow_mut().retain(|pane| *pane != editor_pane));
    });
}

async fn run(shell: Shell, editor_pane: usize) {
    loop {
        let Some(range) = PENDING.with(|pending| pending.borrow_mut().remove(&editor_pane)) else {
            return;
        };
        let Some(handle) = shell.document_of(editor_pane) else {
            return;
        };
        let count = range.len().min(CHUNK_LINES);
        let Ok(lines) = ipc::read_lines(handle, range.start, count).await else {
            return;
        };
        if lines.is_empty() || !editor::feed_pane(editor_pane, range.start, &lines) {
            return;
        }
        // 1 回で入りきらなかった残りは要求へ戻す。次の周で他の要求と合流する。
        let rest = range.start + lines.len()..range.end;
        if !rest.is_empty() {
            PENDING.with(|pending| {
                let mut pending = pending.borrow_mut();
                let merged = match pending.get(&editor_pane) {
                    Some(current) => current.start.min(rest.start)..current.end.max(rest.end),
                    None => rest,
                };
                pending.insert(editor_pane, merged);
            });
        }
    }
}
