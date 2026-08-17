//! Drafts: what is on screen, kept aside so that nothing is lost when the
//! application does not get to say goodbye.
//!
//! The document's own file is written only when the user saves. A draft is a
//! copy beside the settings, written shortly after the typing stops, and
//! removed as soon as it has nothing left to say — the file has been saved, or
//! the work has been thrown away on purpose. What is found at startup is
//! opened as unsaved tabs.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;

use super::shell::Tab;
use crate::editor;
use crate::ipc;

/// How long the typing has to stop before the draft is written, so that a
/// draft costs one write per pause rather than one per keystroke.
const IDLE_MS: i32 = 1200;

thread_local! {
    /// The documents that have changed since the last write, by tab.
    static PENDING: RefCell<HashMap<usize, (Tab, usize)>> = RefCell::new(HashMap::new());
    /// Whether a write is already on its way, so a burst of typing arms one
    /// timer and not one per change.
    static ARMED: Cell<bool> = const { Cell::new(false) };
}

/// Notes that the document of `editor_pane` changed.
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

/// Writes every document that has changed since the last write.
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

/// Writes one document's draft now, without waiting for the pause. Used when
/// the document is about to leave the screen.
pub(super) fn write(tab: Tab, editor_pane: usize) {
    let Some(contents) = editor::document_of(editor_pane) else {
        return;
    };
    PENDING.with(|pending| pending.borrow_mut().remove(&tab.id.get_untracked()));
    let id = tab.id.get_untracked();
    let path = tab.path.get_untracked();
    spawn_local(async move { ipc::write_draft(id, path.as_deref(), &contents).await });
}

/// A document that matches its file, or one thrown away on purpose, has
/// nothing to restore.
pub(super) fn forget(tab: Tab) {
    let id = tab.id.get_untracked();
    PENDING.with(|pending| pending.borrow_mut().remove(&id));
    spawn_local(async move { ipc::remove_draft(id).await });
}
