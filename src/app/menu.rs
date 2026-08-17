//! What the items of the system's menus do.
//!
//! The menus themselves are the operating system's own, built in `src-tauri`;
//! choosing an item only sends its name here. The keys go through the same
//! table (`super::keys`), so a menu item and its shortcut are one thing.

use leptos::prelude::*;
use leptos::task::spawn_local;

use super::preferences::change;
use super::shell::{Field, Shell};
use crate::editor;
use crate::ipc;
use crate::settings;
use crate::settings::Settings;

/// Starts listening for what is picked from the menu bar, and tells the menu
/// what is currently on.
pub(super) fn install(shell: Shell) {
    ipc::on_menu(move |name| choose(shell, name, From::Menu));
    show_state(shell);
}

/// Which road an item arrived by.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum From {
    /// Picked from the menu bar, or its accelerator.
    Menu,
    /// The key pressed in the window.
    Key,
}

/// Carries out one item of the menu.
pub(super) fn choose(shell: Shell, name: &str, from: From) {
    if echo(name, from) {
        return;
    }
    let current = settings::current();
    match name {
        "new" => shell.new_document(),
        "open" => shell.open(),
        "save" => shell.save(false),
        "save_as" => shell.save(true),
        "close_tab" => {
            let pane = shell.pane_untracked();
            shell.close(pane, pane.current.get_untracked());
        }
        "preferences" => shell.preferences.update(|open| *open = !*open),
        "undo" => editor::undo(),
        "redo" => editor::redo(),
        "find" => shell.find(Field::Query),
        "replace" => shell.find(Field::Replacement),
        "insert_math" => editor::insert_math(),
        "wrap" => change(Settings {
            wrap: !current.wrap,
            ..current
        }),
        "line_numbers" => change(Settings {
            line_numbers: !current.line_numbers,
            ..current
        }),
        "split" => shell.toggle_split(),
        _ => return,
    }
    show_state(shell);
}

/// Puts the check marks of the 表示 menu where the settings and the panes are.
pub(super) fn show_state(shell: Shell) {
    let settings = settings::current();
    let split = shell.panes.with_untracked(Vec::len) > 1;
    spawn_local(async move {
        ipc::sync_view_menu(settings.wrap, settings.line_numbers, split).await;
    });
}

/// How long the other road's copy of one keystroke may take to arrive.
const SAME_KEYSTROKE_MS: f64 = 150.0;

thread_local! {
    /// The item carried out last: when, which, and by which road.
    static LAST: std::cell::RefCell<(f64, String, bool)> =
        const { std::cell::RefCell::new((f64::MIN, String::new(), false)) };
}

/// Whether this is the other road's copy of an item just carried out.
///
/// A shortcut can reach the application by two roads: the menu's accelerator
/// and the key itself in the window. Which of the two arrives depends on the
/// platform and on what has the focus, so both are accepted and the copy that
/// comes by the *other* road within a keystroke's time is dropped. Coming again
/// by the same road is a key held down or pressed again, which must go through:
/// holding Ctrl+Z has to keep undoing.
fn echo(name: &str, from: From) -> bool {
    let now = js_sys::Date::now();
    let menu = from == From::Menu;
    LAST.with(|last| {
        let mut last = last.borrow_mut();
        if last.1 == name && last.2 != menu && now - last.0 < SAME_KEYSTROKE_MS {
            return true;
        }
        *last = (now, name.to_string(), menu);
        false
    })
}
