//! アプリケーションのキー: ファイル、タブ、ペイン、検索バー、元に戻す。ドキュメント自体におけるキーストロークの意味は、ここに到達する前にキーを処理する `crate::editor` 独自のテーブルです。
//!
//! メニュー バーにもあるキーは `super::menu` によって実行されるため、キーとメニュー項目は決して別のものではありません。

use leptos::prelude::*;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::KeyboardEvent;

use super::menu;
use super::shell::Shell;

pub(super) fn install_shortcuts(shell: Shell) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let handler = Closure::<dyn FnMut(KeyboardEvent)>::new(move |event: KeyboardEvent| {
        // 編集中の式は独自の履歴を保持し、キーがここに到達する前にキー自体を処理します。
        if event.default_prevented() {
            return;
        }
        if !(event.ctrl_key() || event.meta_key()) {
            if event.key() == "Escape" {
                crate::editor::clear_search_preview();
                shell.panes.with_untracked(|panes| {
                    for pane in panes {
                        pane.searching.set(false);
                    }
                });
                shell.searching.set(false);
            }
            return;
        }
        let shift = event.shift_key();
        // メニュー バーの項目は、横にあるキーによって決まります。
        let item = match (event.key().to_lowercase().as_str(), shift) {
            ("n", _) | ("t", _) => Some("new"),
            ("o", _) => Some("open"),
            ("s", false) => Some("save"),
            ("s", true) => Some("save_as"),
            ("f", _) => Some("find"),
            ("r", _) => Some("replace"),
            ("w", _) => Some("close_tab"),
            ("\\", _) => Some("split"),
            ("m", _) => Some("insert_structure"),
            ("z", false) => Some("undo"),
            ("z", true) | ("y", _) => Some("redo"),
            (",", _) => Some("preferences"),
            _ => None,
        };
        if let Some(item) = item {
            event.prevent_default();
            menu::choose(shell, item, menu::From::Key);
            return;
        }
        // メニュー バーにはタブ間の移動を行う場所がないため、ここに留まります。
        if event.key().to_lowercase() == "tab" {
            event.prevent_default();
            let pane = shell.pane_untracked();
            let count = pane.tabs.with_untracked(Vec::len);
            let current = pane.current.get_untracked();
            let next = if shift {
                (current + count - 1) % count
            } else {
                (current + 1) % count
            };
            shell.switch(pane, next);
        }
    });
    window
        .add_event_listener_with_callback("keydown", handler.as_ref().unchecked_ref())
        .ok();
    handler.forget();
}
