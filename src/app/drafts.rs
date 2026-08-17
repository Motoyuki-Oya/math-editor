//! 下書き: 画面上にあるもの。アプリケーションが別れを告げるまでに何も失われないように脇に保管されます。
//!
//! ドキュメント自体のファイルは、ユーザーが保存するときにのみ書き込まれます。下書きとは、設定の横にあるコピーで、入力が止まった直後に書かれ、何も言うことがなくなるとすぐに削除されます。つまり、ファイルが保存されているか、作業が意図的に破棄されています。起動時に見つかったものは、未保存のタブとして開かれます。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;

use super::shell::Tab;
use crate::editor;
use crate::ipc;

/// 下書きが書き込まれるまでに入力を停止する必要がある時間。そのため、下書きにはキーストロークごとに 1 回ではなく、一時停止ごとに 1 回の書き込みがかかります。
const IDLE_MS: i32 = 1200;

thread_local! {
    /// 最後の書き込み以降に変更されたドキュメント (タブごと)。
    static PENDING: RefCell<HashMap<usize, (Tab, usize)>> = RefCell::new(HashMap::new());
    /// 書き込みがすでに開始されているかどうか。そのため、バースト入力は変更ごとに 1 回ではなく 1 回のタイマーになります。
    static ARMED: Cell<bool> = const { Cell::new(false) };
}

/// `editor_pane` のドキュメントに注意してください。
pub(super) fn touch(tab: Tab, editor_pane: usize) {
    PENDING.with(|pending| {
        pending
            .borrow_mut()
            .insert(tab.id.get_untracked(), (tab, editor_pane))
    });
    arm();
}

fn arm() {
    if ARMED.get() {
        return;
    }
    let Some(window) = web_sys::window() else {
        return;
    };
    let fire = Closure::once_into_js(move || {
        ARMED.set(false);
        flush();
    });
    if window
        .set_timeout_with_callback_and_timeout_and_arguments_0(fire.unchecked_ref(), IDLE_MS)
        .is_ok()
    {
        ARMED.set(true);
    }
}

/// 前回の書き込み以降に変更されたすべてのドキュメントを書き込みます。
fn flush() {
    let pending: Vec<(Tab, usize)> = PENDING.with(|pending| {
        pending
            .borrow_mut()
            .drain()
            .map(|(_, waiting)| waiting)
            .collect()
    });
    for (tab, editor_pane) in pending {
        write(tab, editor_pane);
    }
}

/// 一時停止を待たずに、1 つのドキュメントの下書きを今すぐ書き込みます。ドキュメントが画面から出ようとするときに使用されます。
pub(super) fn write(tab: Tab, editor_pane: usize) {
    let Some(contents) = editor::document_of(editor_pane) else {
        return;
    };
    PENDING.with(|pending| pending.borrow_mut().remove(&tab.id.get_untracked()));
    let id = tab.id.get_untracked();
    let path = tab.path.get_untracked();
    spawn_local(async move { ipc::write_draft(id, path.as_deref(), &contents).await });
}

/// ファイルと一致するドキュメント、または意図的に破棄されたドキュメントには、復元するものがありません。
pub(super) fn forget(tab: Tab) {
    let id = tab.id.get_untracked();
    PENDING.with(|pending| pending.borrow_mut().remove(&id));
    spawn_local(async move { ipc::remove_draft(id).await });
}
