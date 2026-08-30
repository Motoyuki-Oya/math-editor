//! システムのメニューの項目が行うこと。
//!
//! メニュー自体はオペレーティング システム独自のものであり、`src-tauri` に組み込まれています。項目を選択すると、その名前のみがここに送信されます。キーは同じテーブル (`super::keys`) を通過するため、メニュー項目とそのショートカットは 1 つのものです。

use leptos::prelude::*;
use leptos::task::spawn_local;

use super::preferences::change;
use super::shell::{Field, Shell};
use crate::editor;
use crate::framework::{gui, GuiEvent, GuiFramework, MenuState};
use crate::settings;
use crate::settings::Settings;

/// メニュー バーから選択された内容のリッスンを開始し、現在何がオンであるかをメニューに伝えます。
pub(super) fn install(shell: Shell) {
    let _ = gui().on_event(Box::new(move |event| match event {
        GuiEvent::MenuSelected(name) => choose(shell, &name, From::Menu),
    }));
    show_state(shell);
}

/// 項目がどの道路から到着したか。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum From {
    /// メニュー バーまたはそのアクセラレータから選択されます。
    Menu,
    /// ウィンドウ内で押されたキー。
    Key,
}

/// 項目の 1 つを実行します。
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
        "print" => shell.print(),
        "close_tab" => {
            let pane = shell.pane_untracked();
            shell.close(pane, pane.current.get_untracked());
        }
        "preferences" => shell.preferences.update(|open| *open = !*open),
        "select_all" => editor::select_all(),
        "undo" => super::sync::undo(shell, false),
        "redo" => super::sync::undo(shell, true),
        "find" => shell.find(Field::Query),
        "replace" => shell.find(Field::Replacement),
        "insert_structure" => shell.pane().palette.update(|open| *open = !*open),
        "zoom_in" => settings::zoom_in(),
        "zoom_out" => settings::zoom_out(),
        "zoom_reset" => settings::zoom_reset(),
        "wrap" => change(Settings {
            wrap: !current.wrap,
            ..current
        }),
        "line_numbers" => change(Settings {
            line_numbers: !current.line_numbers,
            ..current
        }),
        "show_whitespace" => change(Settings {
            show_whitespace: !current.show_whitespace,
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
        let _ = gui()
            .set_menu(MenuState {
                wrap: settings.wrap,
                line_numbers: settings.line_numbers,
                show_whitespace: settings.show_whitespace,
                split,
            })
            .await;
    });
}

/// 1 つのキーストロークの他のロードのコピーが到着するまでにかかる時間。
const SAME_KEYSTROKE_MS: f64 = 150.0;

thread_local! {
    /// 最後に実行された項目: いつ、どのロードで、どのロードによって実行されたか。
    static LAST: std::cell::RefCell<(f64, String, bool)> =
        const { std::cell::RefCell::new((f64::MIN, String::new(), false)) };
}

/// これが実行されたばかりの項目の他のロードのコピーであるかどうか。
///
/// ショートカットは、メニューのアクセラレータとウィンドウ内のキー自体の 2 つのロードによってアプリケーションに到達できます。 2 つのどちらが到着するかは、プラットフォームと何にフォーカスがあるかによって異なります。そのため、両方が受け入れられ、キーストローク時間内に *他の* ルートによって到着したコピーは削除されます。同じ道を再び進むには、キーを押し続けるか、もう一度押す必要があります。これを通過する必要があります。Ctrl+Z を押したままにすると、元に戻し続ける必要があります。
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
