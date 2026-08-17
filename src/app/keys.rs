//! The keys of the application: files, tabs, panes, the search bar, undo.
//! What a keystroke means in the document itself is
//! `crate::editor`'s own table, which handles the key before it reaches here.
//!
//! A key that also stands in the menu bar is carried out by `super::menu`, so
//! that the key and the menu item are never two different things.

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
        // The formula being edited keeps its own history, and handles the key
        // itself before it reaches here.
        if event.default_prevented() {
            return;
        }
        if !(event.ctrl_key() || event.meta_key()) {
            if event.key() == "Escape" {
                shell.searching.set(false);
            }
            return;
        }
        let shift = event.shift_key();
        // The items of the menu bar, by the key that stands beside them.
        let item = match (event.key().to_lowercase().as_str(), shift) {
            ("n", _) | ("t", _) => Some("new"),
            ("o", _) => Some("open"),
            ("s", false) => Some("save"),
            ("s", true) => Some("save_as"),
            ("f", _) => Some("find"),
            ("r", _) => Some("replace"),
            ("w", _) => Some("close_tab"),
            ("\\", _) => Some("split"),
            ("m", _) => Some("insert_math"),
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
        // Moving between tabs has no place in the menu bar, so it stays here.
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
