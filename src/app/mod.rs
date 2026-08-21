//! アプリケーション シェル: 構造パレット、検索バー、ステータス バー。メニュー自体はオペレーティング システム独自のものです (`menu` および `src-tauri` を参照)。

mod drafts;
mod find;
mod keys;
mod menu;
mod palette;
mod panes;
mod preferences;
mod shell;
mod sync;

use leptos::prelude::*;
use leptos::reactive::owner::Owner;
use leptos::task::spawn_local;

use find::FindBar;
use palette::Palette;
use panes::PaneView;
use preferences::Preferences;
use shell::{Pane, Shell};

use crate::editor;
use crate::ipc;
use crate::settings;

#[component]
pub fn App() -> impl IntoView {
    let shell = Shell {
        root: StoredValue::new_local(Owner::current().expect("the app has an owner")),
        panes: RwSignal::new(vec![Pane::new(0)]),
        focused: RwSignal::new(0),
        next_key: RwSignal::new(1),
        status: RwSignal::new(String::new()),
        stats: RwSignal::new((0, 1)),
        searching: RwSignal::new(false),
        preferences: RwSignal::new(false),
        palette: RwSignal::new(false),
        find_focus: RwSignal::new(None),
    };

    Effect::new(move |_| {
        editor::set_on_change(std::rc::Rc::new(move |pane| shell.mark_dirty(pane)));
        sync::install(shell);
        keys::install_shortcuts(shell);
        menu::install(shell);
        spawn_local(async move {
            // 起動もユーザーの変更と同じ入口を通ります。適用だけでは、保存されていた行番号や折り返しが最初の画面に出ず、メニューのチェックと食い違います。
            preferences::take_effect(settings::read(&ipc::read_settings().await));
            // 保存された設定がある場所にメニューのチェック マークが付けられます。
            menu::show_state(shell);
            shell.restore_drafts(ipc::read_drafts().await);
            ipc::frontend_ready().await;
        });
    });

    Effect::new(move |_| {
        let title = format!(
            "{}{} — Planetext",
            if shell.tab().dirty.get() { "*" } else { "" },
            shell.file_name()
        );
        if let Some(document) = web_sys::window().and_then(|w| w.document()) {
            document.set_title(&title);
        }
    });

    view! {
        <div class="app">
            <Show when=move || shell.palette.get()>
                <Palette/>
            </Show>

            <Show when=move || shell.searching.get()>
                <FindBar shell=shell/>
            </Show>

            <Show when=move || shell.preferences.get()>
                <Preferences open=shell.preferences/>
            </Show>

            <div class="panes">
                <For each=move || shell.panes.get() key=|pane| pane.key let:pane>
                    <PaneView shell=shell pane=pane/>
                </For>
            </div>

            <div class="statusbar">
                <span>{move || shell.file_name()}</span>
                <span>{move || if shell.tab().dirty.get() { "未保存" } else { "保存済み" }}</span>
                <span>{move || {
                    let (characters, lines) = shell.stats.get();
                    format!("{characters} 文字 / {lines} 行")
                }}</span>
                <span class="status-message">{move || shell.status.get()}</span>
            </div>
        </div>
    }
}

/// ツールバー操作で、現在編集中の入れ子Rowからフォーカスが外れないようにします。
pub(super) fn hold_focus(event: web_sys::MouseEvent) {
    event.prevent_default();
}
