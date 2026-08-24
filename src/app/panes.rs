//! 画面上のペイン: そのタブ ストリップとその下のエディタ。

use leptos::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::HtmlElement;

use super::hold_focus;
use super::palette::Palette;
use super::shell::{self, Pane, Shell};
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
            <Show when=move || pane.palette.get()>
                <Palette/>
            </Show>
            <div class="editor" node_ref=editor_ref></div>
        </div>
    }
}

/// 開いているファイルごとに 1 つのボタンがあり、未保存マークとそれを閉じる方法が表示されます。
#[component]
fn Tabs(shell: Shell, pane: Pane) -> impl IntoView {
    view! {
        <div
            class=move || {
                let is_end_target = shell
                    .tab_drag
                    .get()
                    .and_then(|d| if d.is_dragging { d.drop_target } else { None })
                    .map(|t| t.pane_key == pane.key && t.index >= pane.tabs.with(Vec::len))
                    .unwrap_or(false);
                if is_end_target {
                    "tabbar tabbar-drop-end"
                } else {
                    "tabbar"
                }
            }
            data-tabbar-pane=pane.key
            data-tabbar-count=move || pane.tabs.with(Vec::len)
        >
            {move || {
                let current = pane.current.get();
                let tabs = pane.tabs.get();
                let tabs_len = tabs.len();
                tabs.into_iter()
                    .enumerate()
                    .map(|(index, tab)| {
                        let is_dragging = move || {
                            shell
                                .tab_drag
                                .get()
                                .map(|d| {
                                    d.is_dragging
                                        && d.src_pane_key == pane.key
                                        && d.src_tab_index == index
                                })
                                .unwrap_or(false)
                        };
                        let drop_class = move || {
                            let target = shell
                                .tab_drag
                                .get()
                                .and_then(|d| if d.is_dragging { d.drop_target } else { None });
                            match target {
                                Some(t) if t.pane_key == pane.key && t.index == index => {
                                    " tab-drop-before"
                                }
                                Some(t)
                                    if t.pane_key == pane.key
                                        && t.index == index + 1
                                        && index == tabs_len.saturating_sub(1) =>
                                {
                                    " tab-drop-after"
                                }
                                _ => "",
                            }
                        };
                        view! {
                            <span
                                class=move || {
                                    format!(
                                        "tab{}{}{}",
                                        if index == current { " tab-current" } else { "" },
                                        if is_dragging() { " tab-dragging" } else { "" },
                                        drop_class(),
                                    )
                                }
                                data-tab-pane=pane.key
                                data-tab-index=index
                                on:pointerdown=move |ev: web_sys::PointerEvent| {
                                    if ev.button() != 0 {
                                        return;
                                    }
                                    let name = tab.name();
                                    let dirty = tab.dirty.get_untracked();
                                    shell
                                        .tab_drag
                                        .set(
                                            Some(shell::TabDragState {
                                                src_pane_key: pane.key,
                                                src_tab_index: index,
                                                tab_name: format!(
                                                    "{}{}",
                                                    if dirty { "*" } else { "" },
                                                    name,
                                                ),
                                                start_x: ev.client_x() as f64,
                                                start_y: ev.client_y() as f64,
                                                current_x: ev.client_x() as f64,
                                                current_y: ev.client_y() as f64,
                                                is_dragging: false,
                                                drop_target: None,
                                            }),
                                        );
                                }
                            >
                                <span class="tab-name">
                                    {move || {
                                        format!(
                                            "{}{}",
                                            if tab.dirty.get() { "*" } else { "" },
                                            tab.name(),
                                        )
                                    }}
                                </span>
                                <button
                                    class="tab-close"
                                    title="閉じる (Ctrl+W)"
                                    on:pointerdown=move |ev: web_sys::PointerEvent| ev.stop_propagation()
                                    on:click=move |ev: web_sys::MouseEvent| {
                                        ev.stop_propagation();
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
            <button
                class="tab-palette"
                title="構造パレット (Ctrl+M)"
                on:mousedown=hold_focus
                on:click=move |_| pane.palette.update(|open| *open = !*open)
            >
                "∑"
            </button>
        </div>
    }
}
