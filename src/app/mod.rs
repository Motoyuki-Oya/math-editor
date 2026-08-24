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
        find_focus: RwSignal::new(None),
        tab_drag: RwSignal::new(None),
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
        let Some(window) = web_sys::window() else {
            return;
        };
        let document = window.document();

        use wasm_bindgen::JsCast;

        let doc_clone = document.clone();
        let on_pointer_move = wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::PointerEvent)>::new(move |ev: web_sys::PointerEvent| {
            let Some(mut drag) = shell.tab_drag.get_untracked() else {
                return;
            };
            let x = ev.client_x() as f64;
            let y = ev.client_y() as f64;
            let dist = ((x - drag.start_x).powi(2) + (y - drag.start_y).powi(2)).sqrt();
            if dist > 3.0 {
                drag.is_dragging = true;
            }
            drag.current_x = x;
            drag.current_y = y;

            drag.drop_target = None;
            if drag.is_dragging {
                if let Some(ref doc) = doc_clone {
                    if let Some(el) = doc.element_from_point(x as f32, y as f32) {
                        if let Ok(Some(tab_el)) = el.closest("[data-tab-pane]") {
                            if let (Some(pane_str), Some(idx_str)) = (
                                tab_el.get_attribute("data-tab-pane"),
                                tab_el.get_attribute("data-tab-index"),
                            ) {
                                if let (Ok(pane_key), Ok(tab_idx)) = (
                                    pane_str.parse::<usize>(),
                                    idx_str.parse::<usize>(),
                                ) {
                                    let rect = tab_el.get_bounding_client_rect();
                                    let is_after = x > (rect.left() + rect.width() / 2.0);
                                    let insert_index = if is_after { tab_idx + 1 } else { tab_idx };
                                    drag.drop_target = Some(shell::DropTarget {
                                        pane_key,
                                        index: insert_index,
                                    });
                                }
                            }
                        } else if let Ok(Some(tabbar_el)) = el.closest("[data-tabbar-pane]") {
                            if let (Some(pane_str), Some(count_str)) = (
                                tabbar_el.get_attribute("data-tabbar-pane"),
                                tabbar_el.get_attribute("data-tabbar-count"),
                            ) {
                                if let (Ok(pane_key), Ok(count)) = (
                                    pane_str.parse::<usize>(),
                                    count_str.parse::<usize>(),
                                ) {
                                    drag.drop_target = Some(shell::DropTarget {
                                        pane_key,
                                        index: count,
                                    });
                                }
                            }
                        }
                    }
                }
            }

            shell.tab_drag.set(Some(drag));
        });

        let on_pointer_up = wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::PointerEvent)>::new(move |_ev: web_sys::PointerEvent| {
            let Some(drag) = shell.tab_drag.get_untracked() else {
                return;
            };
            shell.tab_drag.set(None);

            if drag.is_dragging {
                if let Some(target) = drag.drop_target {
                    if let Some(dst_pane) = shell
                        .panes
                        .with_untracked(|panes| panes.iter().find(|p| p.key == target.pane_key).copied())
                    {
                        shell.move_tab(drag.src_pane_key, drag.src_tab_index, dst_pane, target.index);
                    }
                }
            } else {
                if let Some(src_pane) = shell
                    .panes
                    .with_untracked(|panes| panes.iter().find(|p| p.key == drag.src_pane_key).copied())
                {
                    shell.focus_on(src_pane);
                    shell.switch(src_pane, drag.src_tab_index);
                }
            }
        });

        window
            .add_event_listener_with_callback("pointermove", on_pointer_move.as_ref().unchecked_ref())
            .ok();
        window
            .add_event_listener_with_callback("pointerup", on_pointer_up.as_ref().unchecked_ref())
            .ok();
        on_pointer_move.forget();
        on_pointer_up.forget();
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

            <Show when=move || shell.tab_drag.get().map(|d| d.is_dragging).unwrap_or(false)>
                {move || {
                    let drag = shell.tab_drag.get()?;
                    Some(view! {
                        <div
                            class="tab-ghost"
                            style=format!(
                                "left: {}px; top: {}px;",
                                drag.current_x, drag.current_y
                            )
                        >
                            <span>{drag.tab_name}</span>
                        </div>
                    })
                }}
            </Show>
        </div>
    }
}

/// ツールバー操作で、現在編集中の入れ子Rowからフォーカスが外れないようにします。
pub(super) fn hold_focus(event: web_sys::MouseEvent) {
    event.prevent_default();
}
