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
    ipc::on_menu(move |name| choose(shell, name));
    show_state(shell);
}

/// Carries out one item of the menu.
pub(super) fn choose(shell: Shell, name: &str) {
    if repeated(name) {
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

/// How long a second arrival of the same item counts as the same keystroke.
const SAME_KEYSTROKE_MS: f64 = 150.0;

thread_local! {
    /// The item carried out last, and when.
    static LAST: std::cell::RefCell<(f64, String)> =
        const { std::cell::RefCell::new((f64::MIN, String::new())) };
}

/// Whether this is the same item arriving twice for one keystroke.
///
/// A shortcut can reach the application by two roads: the menu's accelerator
/// and the key itself in the window. Which of the two arrives depends on the
/// platform and on what has the focus, so both are accepted and the second one
/// within a keystroke's time is dropped, rather than guessing.
fn repeated(name: &str) -> bool {
    let now = js_sys::Date::now();
    LAST.with(|last| {
        let mut last = last.borrow_mut();
        let same = last.1 == name && now - last.0 < SAME_KEYSTROKE_MS;
        *last = (now, name.to_string());
        same
    })
}
