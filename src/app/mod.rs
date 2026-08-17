//! Application shell: toolbar, structure palette, search bar and status bar.

mod find;
mod keys;
mod palette;
mod panes;
mod preferences;
mod shell;

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
        find_focus: RwSignal::new(None),
    };
    let preferences_open = RwSignal::new(false);

    Effect::new(move |_| {
        editor::set_on_change(Box::new(move |pane| shell.mark_dirty(pane)));
        keys::install_shortcuts(shell);
        spawn_local(async {
            settings::apply(settings::read(&ipc::read_settings().await));
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
            <div class="toolbar">
                <div class="group">
                    <button class="tool" on:mousedown=hold_focus on:click=move |_| shell.new_document() title="新しいタブ (Ctrl+T)">"新規"</button>
                    <button class="tool" on:mousedown=hold_focus on:click=move |_| shell.open()>"開く"</button>
                    <button class="tool" on:mousedown=hold_focus on:click=move |_| shell.save(false)>"保存"</button>
                    <button class="tool" on:mousedown=hold_focus on:click=move |_| shell.save(true)>"名前を付けて"</button>
                </div>
                <div class="group">
                    <button class="tool" on:mousedown=hold_focus on:click=move |_| editor::insert_math() title="構造を挿入 (Ctrl+M)">"構造"</button>
                    <button class="tool" on:mousedown=hold_focus on:click=move |_| shell.searching.update(|s| *s = !*s) title="検索と置換 (Ctrl+F / 置換は Ctrl+R)">"検索"</button>
                    <button
                        class="tool"
                        on:mousedown=hold_focus
                        on:click=move |_| shell.toggle_split()
                        title="左右に分割 / 解除 (Ctrl+\\)"
                    >
                        {move || if shell.panes.get().len() > 1 { "分割解除" } else { "分割" }}
                    </button>
                    <button class="tool" on:mousedown=hold_focus on:click=move |_| preferences_open.update(|open| *open = !*open)>"設定"</button>
                </div>
            </div>

            <Palette/>

            <Show when=move || shell.searching.get()>
                <FindBar shell=shell/>
            </Show>

            <Show when=move || preferences_open.get()>
                <Preferences open=preferences_open/>
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

/// Keeps the caret inside the formula when a toolbar button is pressed.
pub(super) fn hold_focus(event: web_sys::MouseEvent) {
    event.prevent_default();
}
