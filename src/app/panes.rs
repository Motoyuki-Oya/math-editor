//! A pane on screen: its tab strip and the editor below it.

use leptos::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::HtmlElement;

use super::hold_focus;
use super::shell::{Pane, Shell};
use crate::editor;

/// A tab strip and the editor below it.
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
        // A pane made by splitting takes the typing right away.
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

/// One button per open file, with the unsaved mark and a way to close it.
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
