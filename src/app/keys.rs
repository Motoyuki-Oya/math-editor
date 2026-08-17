//! The keys of the application: files, tabs, panes, the search bar, undo.
//! What a keystroke means in the document itself is
//! `crate::editor`'s own table, which handles the key before it reaches here.

use leptos::prelude::*;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::KeyboardEvent;

use super::shell::{Field, Shell};
use crate::editor;

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
        match event.key().to_lowercase().as_str() {
            "n" => {
                event.prevent_default();
                shell.new_document();
            }
            "o" => {
                event.prevent_default();
                shell.open();
            }
            "s" => {
                event.prevent_default();
                shell.save(shift);
            }
            "f" => {
                event.prevent_default();
                shell.find(Field::Query);
            }
            "r" => {
                event.prevent_default();
                shell.find(Field::Replacement);
            }
            "t" => {
                event.prevent_default();
                shell.new_document();
            }
            "w" => {
                event.prevent_default();
                let pane = shell.pane_untracked();
                shell.close(pane, pane.current.get_untracked());
            }
            "\\" => {
                event.prevent_default();
                shell.toggle_split();
            }
            "tab" => {
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
            "m" => {
                event.prevent_default();
                editor::insert_math();
            }
            "z" => {
                event.prevent_default();
                if shift {
                    editor::redo();
                } else {
                    editor::undo();
                }
            }
            "y" => {
                event.prevent_default();
                editor::redo();
            }
            _ => {}
        }
    });
    window
        .add_event_listener_with_callback("keydown", handler.as_ref().unchecked_ref())
        .ok();
    handler.forget();
}
