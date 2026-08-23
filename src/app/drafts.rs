//! 下書き: 画面上にあるもの。アプリケーションが別れを告げるまでに何も失われないように脇に保管されます。
//!
//! ドキュメント自体のファイルは、ユーザーが保存するときにのみ書き込まれます。下書きとは、設定の横にあるコピーで、入力が止まった直後に書かれ、何も言うことがなくなるとすぐに削除されます。つまり、ファイルが保存されているか、作業が意図的に破棄されています。起動時に見つかったものは、未保存のタブとして開かれます。
//!
//! 書き込み自体は文書の本体（ネイティブ側）が行い、ここは「いつ書くか」だけを
//! 決めます。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;

use super::shell::Tab;
use super::sync;
use crate::ipc;

/// 下書きが書き込まれるまでに入力を停止する必要がある時間。そのため、下書きにはキーストロークごとに 1 回ではなく、一時停止ごとに 1 回の書き込みがかかります。
const IDLE_MS: i32 = 1200;

thread_local! {
    /// 最後の書き込み以降に変更されたドキュメント (タブごと)。
    static PENDING: RefCell<HashMap<usize, Tab>> = RefCell::new(HashMap::new());
    /// 書き込みがすでに開始されているかどうか。そのため、バースト入力は変更ごとに 1 回ではなく 1 回のタイマーになります。
    static ARMED: Cell<bool> = const { Cell::new(false) };
}

/// タブのドキュメントが変わったことに注意してください。
pub(super) fn touch(tab: Tab) {
    // 下書きは全文を書き出すので、大きすぎる文書には書かない。
    if tab.large.get_untracked() {
        return;
    }
    PENDING.with(|pending| pending.borrow_mut().insert(tab.id.get_untracked(), tab));
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
    let pending: Vec<Tab> =
        PENDING.with(|pending| pending.borrow_mut().drain().map(|(_, tab)| tab).collect());
    for tab in pending {
        sync::draft(tab);
    }
}

/// ファイルと一致するドキュメント、または意図的に破棄されたドキュメントには、復元するものがありません。
pub(super) fn forget(tab: Tab) {
    let id = tab.id.get_untracked();
    PENDING.with(|pending| pending.borrow_mut().remove(&id));
    spawn_local(async move { ipc::remove_draft(id).await });
}
