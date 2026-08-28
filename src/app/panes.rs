//! 画面上のペイン: そのタブ ストリップとその下のエディタ。

use leptos::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::HtmlElement;

use super::find::FindBar;
use super::hold_focus;
use super::palette::Palette;
use super::shell::{self, Pane, Shell};
use crate::editor;
use crate::ipc;
use leptos::task::spawn_local;

/// タブのコンテキストメニュー状態。
#[derive(Clone, Copy, Debug, PartialEq)]
struct ContextMenuState {
    x: f64,
    y: f64,
    pane_key: usize,
    tab_index: usize,
}

/// タブストリップとエディタの下にある。
#[component]
pub(super) fn PaneView(
    shell: Shell,
    pane: Pane,
    #[prop(into, optional)] style: Option<Signal<String>>,
) -> impl IntoView {
    let editor_ref = NodeRef::<leptos::html::Div>::new();
    let context_menu = RwSignal::new(None::<ContextMenuState>);

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
        let editor_pane = editor::init(&element);
        pane.editor.set_value(editor_pane);
        if let Some(ep) = editor_pane {
            let current_tab = pane.tab_untracked();
            editor::bind_doc(ep, current_tab.id.get_untracked());
            shell.show(pane, current_tab);
        }
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

    use std::rc::Rc;

    let url_tooltip = RwSignal::new(None::<editor::UrlTooltip>);
    let check_url = move || {
        if let Some(ep) = pane.editor.get_value() {
            url_tooltip.set(editor::url_at_caret(ep));
        } else {
            url_tooltip.set(None);
        }
    };

    Effect::new(move |_| {
        if let Some(ep) = pane.editor.get_value() {
            editor::add_on_redraw(Rc::new(move |id| {
                if id == ep {
                    url_tooltip.set(editor::url_at_caret(ep));
                }
            }));
        }
    });

    let open_url = move |url: String| {
        spawn_local(async move {
            ipc::open_external_url(&url).await;
        });
    };

    view! {
        <div
            class=move || if focused() { "pane pane-focused" } else { "pane" }
            style=move || style.as_ref().map(|s| s.get()).unwrap_or_else(|| "flex: 1 1 0px;".to_string())
            on:mousedown=move |_| {
                context_menu.set(None);
                shell.note_focus(pane);
            }
            on:focusin=move |_| shell.note_focus(pane)
        >
            <Tabs shell=shell pane=pane context_menu=context_menu/>
            <Show when=move || pane.palette.get()>
                <Palette/>
            </Show>
            <div class="editor-container">
                <Show when=move || pane.searching.get()>
                    <FindBar shell=shell pane=pane/>
                </Show>
                <div
                    class="editor"
                    node_ref=editor_ref
                    on:keyup=move |_| check_url()
                    on:pointerup=move |_| check_url()
                    on:wheel=move |_| check_url()
                    on:scroll=move |_| check_url()
                ></div>
                <Show when=move || url_tooltip.get().is_some()>
                    {move || {
                        let tooltip = url_tooltip.get()?;
                        let url_str = tooltip.url.clone();
                        let url_click = tooltip.url.clone();
                        Some(view! {
                            <div
                                class="url-tooltip"
                                style=format!("left: {}px; top: {}px;", tooltip.left.max(8.0), tooltip.top.max(8.0))
                                on:mousedown=move |ev| ev.stop_propagation()
                            >
                                <button
                                    class="url-tooltip-button"
                                    title=url_str
                                    on:click=move |ev| {
                                        ev.prevent_default();
                                        ev.stop_propagation();
                                        open_url(url_click.clone());
                                    }
                                >
                                    <span class="url-tooltip-icon">"🔗"</span>
                                    <span>"リンクを開く ↗"</span>
                                </button>
                            </div>
                        })
                    }}
                </Show>
            </div>
            <Show when=move || context_menu.get().is_some()>
                <TabContextMenu shell=shell pane=pane state=context_menu/>
            </Show>
        </div>
    }
}

/// タブの右クリックメニュー。
#[component]
fn TabContextMenu(
    shell: Shell,
    pane: Pane,
    state: RwSignal<Option<ContextMenuState>>,
) -> impl IntoView {
    let menu_ref = NodeRef::<leptos::html::Div>::new();

    let close_menu = move || state.set(None);

    view! {
        <div
            class="tab-context-menu-backdrop"
            on:mousedown=move |ev| {
                ev.stop_propagation();
                close_menu();
            }
            on:contextmenu=move |ev| {
                ev.prevent_default();
                close_menu();
            }
        >
            <div
                class="tab-context-menu"
                node_ref=menu_ref
                style=move || {
                    if let Some(s) = state.get() {
                        format!("left: {}px; top: {}px;", s.x, s.y)
                    } else {
                        "display: none;".to_string()
                    }
                }
                on:mousedown=move |ev| ev.stop_propagation()
            >
                <button
                    class="context-menu-item"
                    on:click=move |_| {
                        if let Some(s) = state.get_untracked() {
                            shell.split_tab(pane, s.tab_index);
                        }
                        close_menu();
                    }
                >
                    "右に分割して開く"
                </button>
                <div class="context-menu-separator"/>
                <button
                    class="context-menu-item"
                    on:click=move |_| {
                        if let Some(s) = state.get_untracked() {
                            shell.close(pane, s.tab_index);
                        }
                        close_menu();
                    }
                >
                    "閉じる"
                </button>
                <button
                    class="context-menu-item"
                    on:click=move |_| {
                        if let Some(s) = state.get_untracked() {
                            shell.close_other_tabs(pane, s.tab_index);
                        }
                        close_menu();
                    }
                >
                    "他のタブを閉じる"
                </button>
                <button
                    class="context-menu-item"
                    on:click=move |_| {
                        if let Some(s) = state.get_untracked() {
                            shell.close_tabs_to_right(pane, s.tab_index);
                        }
                        close_menu();
                    }
                >
                    "右側のタブを閉じる"
                </button>
            </div>
        </div>
    }
}

/// 開いているファイルごとに 1 つのボタンがあり、未保存マークとそれを閉じる方法が表示されます。
#[component]
fn Tabs(
    shell: Shell,
    pane: Pane,
    context_menu: RwSignal<Option<ContextMenuState>>,
) -> impl IntoView {
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
                                on:contextmenu=move |ev: web_sys::MouseEvent| {
                                    ev.prevent_default();
                                    ev.stop_propagation();
                                    shell.focus_on(pane);
                                    context_menu.set(Some(ContextMenuState {
                                        x: ev.client_x() as f64,
                                        y: ev.client_y() as f64,
                                        pane_key: pane.key,
                                        tab_index: index,
                                    }));
                                }
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
                class="tab-split"
                title="右に分割して開く (Ctrl+\\)"
                on:mousedown=hold_focus
                on:click=move |_| {
                    shell.focus_on(pane);
                    let cur = pane.current.get_untracked();
                    shell.split_tab(pane, cur);
                }
            >
                "◫"
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
