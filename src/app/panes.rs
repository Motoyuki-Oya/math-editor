//! 画面上のペイン: そのタブ ストリップとその下のエディタ。

use leptos::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::HtmlElement;

use super::hold_focus;
use super::shell::{Pane, Shell};
use crate::editor;

/// タブストリップとエディタの下にある。
#[component]
pub(super) fn PaneView(shell: Shell, pane: Pane) -> impl IntoView {
    let editor_ref = NodeRef::<leptos::html::Div>::new();

    Effect::new(move |_| {
        let Some(element) = editor_ref.get() else {
            return;
        };
        let Ok(element) = element.dyn_into::<HtmlElement>() else {
            return;
        };
        if pane.editor.get_value().is_some() {
            return;
        }
        pane.editor.set_value(editor::init(&element));
        // スプリットで作られたパンは、すぐにタイピングを取ります。
        if let Some(index) = shell.index_of(pane) {
            if index == shell.focused.get_untracked() {
                shell.focus_pane(index);
            }
        }
    });

    let focused = move || {
        shell
            .panes
            .with(|panes| panes.get(shell.focused.get()).map(|other| other.key))
            == Some(pane.key)
    };

    view! {
        <div
            class=move || if focused() { "pane pane-focused" } else { "pane" }
            on:mousedown=move |_| shell.note_focus(pane)
            on:focusin=move |_| shell.note_focus(pane)
        >
            <Tabs shell=shell pane=pane/>
            <div class="editor" node_ref=editor_ref></div>
        </div>
    }
}

/// 開いているファイルごとに 1 つのボタンがあり、未保存マークとそれを閉じる方法が表示されます。
#[component]
fn Tabs(shell: Shell, pane: Pane) -> impl IntoView {
    view! {
        <div class="tabbar">
            {move || {
                let current = pane.current.get();
                pane.tabs
                    .get()
                    .into_iter()
                    .enumerate()
                    .map(|(index, tab)| {
                        view! {
                            <span class=move || {
                                if index == current { "tab tab-current" } else { "tab" }
                            }>
                                <button
                                    class="tab-name"
                                    on:mousedown=hold_focus
                                    on:click=move |_| {
                                        shell.focus_on(pane);
                                        shell.switch(pane, index);
                                    }
                                >
                                    {move || {
                                        format!(
                                            "{}{}",
                                            if tab.dirty.get() { "*" } else { "" },
                                            tab.name(),
                                        )
                                    }}
                                </button>
                                <button
                                    class="tab-close"
                                    title="閉じる (Ctrl+W)"
                                    on:mousedown=hold_focus
                                    on:click=move |_| {
                                        shell.focus_on(pane);
                                        shell.close(pane, index);
                                    }
                                >
                                    "×"
                                </button>
                            </span>
                        }
                    })
                    .collect::<Vec<_>>()
            }}
            <button
                class="tab-add"
                title="新しいタブ (Ctrl+T)"
                on:mousedown=hold_focus
                on:click=move |_| {
                    shell.focus_on(pane);
                    shell.new_document();
                }
            >
                "+"
            </button>
        </div>
    }
}
