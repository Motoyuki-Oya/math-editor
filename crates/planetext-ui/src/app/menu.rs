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
    /// メニュー バーから選択されます（マウスクリック等）。
    Menu,
    /// ウィンドウ内で押されたキー。
    Key,
}

/// 項目の 1 つを実行します。
pub(super) fn choose(shell: Shell, name: &str, _from: From) {
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

/// 設定変更時にシェル参照なしでメニューのチェック状態を同期します。
pub(super) fn update_menu_state() {
    let settings = settings::current();
    let split = editor::pane_count() > 1;
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
