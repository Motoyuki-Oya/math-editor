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

use panes::PaneView;
use preferences::Preferences;
use shell::{Pane, Shell};

use crate::editor;
use crate::framework::{gui, read_drafts, read_session_state, read_settings, GuiFramework};
use crate::settings;

#[component]
pub fn App() -> impl IntoView {
    let shell = Shell {
        root: StoredValue::new_local(Owner::current().expect("the app has an owner")),
        panes: RwSignal::new(vec![Pane::new(0)]),
        focused: RwSignal::new(0),
        next_key: RwSignal::new(1),
        status: RwSignal::new(String::new()),
        stats: RwSignal::new(editor::DocStats::default()),
        searching: RwSignal::new(false),
        preferences: RwSignal::new(false),
        find_focus: RwSignal::new(None),
        tab_drag: RwSignal::new(None),
        split_ratio: RwSignal::new(0.5),
        resizing_split: RwSignal::new(false),
        restored: RwSignal::new(false),
    };

    Effect::new(move |_| {
        editor::set_on_change(std::rc::Rc::new(move |pane| shell.mark_dirty(pane)));
        editor::set_on_focus(std::rc::Rc::new(move |pane| {
            shell.note_focus_by_editor_pane(pane)
        }));
        editor::add_on_redraw(std::rc::Rc::new(move |_| {
            shell.refresh();
        }));
        sync::install(shell);
        keys::install_shortcuts(shell);
        menu::install(shell);
        spawn_local(async move {
            // 起動もユーザーの変更と同じ入口を通ります。適用だけでは、保存されていた行番号や折り返しが最初の画面に出ず、メニューのチェックと食い違います。
            preferences::take_effect(settings::read(&read_settings().await));
            // 保存された設定がある場所にメニューのチェック マークが付けられます。
            menu::show_state(shell);
            let drafts = read_drafts().await;
            let session_json = read_session_state().await;
            shell.restore_workspace(session_json, drafts);
            let _ = gui().ready().await;
        });
    });

    Effect::new(move |_| {
        let Some(window) = web_sys::window() else {
            return;
        };
        let document = window.document();

        use wasm_bindgen::JsCast;

        let doc_clone = document.clone();
        let on_pointer_move =
            wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::PointerEvent)>::new(
                move |ev: web_sys::PointerEvent| {
                    if shell.resizing_split.get_untracked() {
                        if let Some(ref doc) = doc_clone {
                            if let Some(panes_el) = doc.query_selector(".panes").ok().flatten() {
                                let rect = panes_el.get_bounding_client_rect();
                                let width = rect.width();
                                if width > 0.0 {
                                    let ratio = ((ev.client_x() as f64 - rect.left()) / width)
                                        .clamp(0.1, 0.9);
                                    shell.split_ratio.set(ratio);
                                }
                            }
                        }
                        return;
                    }

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
                                        if let (Ok(pane_key), Ok(tab_idx)) =
                                            (pane_str.parse::<usize>(), idx_str.parse::<usize>())
                                        {
                                            let rect = tab_el.get_bounding_client_rect();
                                            let is_after = x > (rect.left() + rect.width() / 2.0);
                                            let insert_index =
                                                if is_after { tab_idx + 1 } else { tab_idx };
                                            drag.drop_target = Some(shell::DropTarget {
                                                pane_key,
                                                index: insert_index,
                                            });
                                        }
                                    }
                                } else if let Ok(Some(tabbar_el)) = el.closest("[data-tabbar-pane]")
                                {
                                    if let (Some(pane_str), Some(count_str)) = (
                                        tabbar_el.get_attribute("data-tabbar-pane"),
                                        tabbar_el.get_attribute("data-tabbar-count"),
                                    ) {
                                        if let (Ok(pane_key), Ok(count)) =
                                            (pane_str.parse::<usize>(), count_str.parse::<usize>())
                                        {
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
                },
            );

        let on_pointer_up = wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::PointerEvent)>::new(
            move |_ev: web_sys::PointerEvent| {
                if shell.resizing_split.get_untracked() {
                    shell.resizing_split.set(false);
                    editor::redraw_all();
                    shell.save_session();
                    return;
                }

                let Some(drag) = shell.tab_drag.get_untracked() else {
                    return;
                };
                shell.tab_drag.set(None);

                if drag.is_dragging {
                    if let Some(target) = drag.drop_target {
                        if let Some(dst_pane) = shell.panes.with_untracked(|panes| {
                            panes.iter().find(|p| p.key == target.pane_key).copied()
                        }) {
                            shell.move_tab(
                                drag.src_pane_key,
                                drag.src_tab_index,
                                dst_pane,
                                target.index,
                            );
                        }
                    }
                } else {
                    if let Some(src_pane) = shell.panes.with_untracked(|panes| {
                        panes.iter().find(|p| p.key == drag.src_pane_key).copied()
                    }) {
                        shell.focus_on(src_pane);
                        shell.switch(src_pane, drag.src_tab_index);
                    }
                }
            },
        );

        let on_wheel = wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::WheelEvent)>::new(
            move |ev: web_sys::WheelEvent| {
                if ev.ctrl_key() || ev.meta_key() {
                    ev.prevent_default();
                }
            },
        );
        window
            .add_event_listener_with_callback("wheel", on_wheel.as_ref().unchecked_ref())
            .ok();
        on_wheel.forget();

        let on_keydown = wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(
            move |ev: web_sys::KeyboardEvent| {
                if (ev.ctrl_key() || ev.meta_key())
                    && (ev.key() == "=" || ev.key() == "+" || ev.key() == "-" || ev.key() == "0")
                {
                    ev.prevent_default();
                }
            },
        );
        window
            .add_event_listener_with_callback("keydown", on_keydown.as_ref().unchecked_ref())
            .ok();
        on_keydown.forget();

        window
            .add_event_listener_with_callback(
                "pointermove",
                on_pointer_move.as_ref().unchecked_ref(),
            )
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

    let encoding_menu = RwSignal::new(None::<(f64, f64)>);
    let line_ending_menu = RwSignal::new(None::<(f64, f64)>);
    let language_menu = RwSignal::new(None::<(f64, f64)>);

    view! {
        <div class="app" on:contextmenu=move |ev| ev.prevent_default()>
            <Show when=move || shell.preferences.get()>
                <Preferences open=shell.preferences/>
            </Show>

            <div class="panes">
                <For
                    each=move || shell.panes.get()
                    key=|pane| pane.key
                    let:pane
                >
                    {
                        let pane_key = pane.key;
                        let is_resizing = shell.resizing_split;
                        let is_first = Signal::derive(move || {
                            shell
                                .panes
                                .with(|p| p.first().map(|x| x.key == pane_key).unwrap_or(true))
                        });
                        let style = Signal::derive(move || {
                            let count = shell.panes.with(Vec::len);
                            if count == 2 {
                                let r = shell.split_ratio.get();
                                if is_first.get() {
                                    format!("flex: {r} 1 0px;")
                                } else {
                                    format!("flex: {} 1 0px;", 1.0 - r)
                                }
                            } else {
                                "flex: 1 1 0px;".to_string()
                            }
                        });
                        view! {
                            <Show when=move || !is_first.get()>
                                <div
                                    class=move || {
                                        if is_resizing.get() {
                                            "pane-divider pane-divider-active"
                                        } else {
                                            "pane-divider"
                                        }
                                    }
                                    on:pointerdown=move |ev: web_sys::PointerEvent| {
                                        if ev.button() == 0 {
                                            ev.prevent_default();
                                            shell.resizing_split.set(true);
                                        }
                                    }
                                    on:dblclick=move |_| {
                                        shell.split_ratio.set(0.5);
                                        editor::redraw_all();
                                    }
                                />
                            </Show>
                            <PaneView shell=shell pane=pane style=style/>
                        }
                    }
                </For>
            </div>

            <div class="statusbar">
                <div class="statusbar-left">
                    <button
                        class=move || if shell.preferences.get() { "status-icon-btn active" } else { "status-icon-btn" }
                        title="設定 (Ctrl+,)"
                        on:click=move |_| shell.preferences.update(|open| *open = !*open)
                    >
                        "⚙"
                    </button>
                    <span>{move || {
                        let stats = shell.stats.get();
                        let tab = shell.tab();
                        let bytes = tab.bytes.get();
                        if let Some(sel) = &stats.selection {
                            if let Some((chars_without_nl, newlines)) = sel.chars {
                                let total_sel = chars_without_nl + newlines;
                                format!("選択 {total_sel}文字")
                            } else {
                                format!("選択 {}行", sel.lines)
                            }
                        } else if let Some(chars) = stats.total_chars {
                            format!("全 {chars}文字")
                        } else if stats.counting {
                            "全 0文字".to_string()
                        } else {
                            format_file_size(bytes)
                        }
                    }}</span>
                    <Show when=move || shell.stats.get().caret_prefix.is_some()>
                        {move || {
                            let stats = shell.stats.get();
                            let (chars_without_nl, newlines) = stats.caret_prefix?;
                            let total_caret = chars_without_nl + newlines;
                            Some(view! {
                                <span>{format!("先頭から{total_caret} ( 📄{chars_without_nl} ⏎ {newlines} )")}</span>
                            })
                        }}
                    </Show>
                </div>

                <div class="statusbar-center">
                    <span class="status-message">{move || shell.status.get()}</span>
                </div>

                <div class="statusbar-right">
                    <button
                        type="button"
                        class="status-clickable"
                        prop:disabled=move || {
                            let _ = shell.stats.get();
                            !crate::settings::current().enable_overwrite_mode && !editor::is_focused_overwrite_mode()
                        }
                        title=move || {
                            let _ = shell.stats.get();
                            let is_overwrite = editor::is_focused_overwrite_mode();
                            let enabled = crate::settings::current().enable_overwrite_mode;
                            if !enabled && !is_overwrite {
                                "入力モード: 挿入 (上書きモードは設定で無効化されています)".to_string()
                            } else if is_overwrite {
                                "入力モード: 上書き (クリックまたはInsertキーで挿入に切替)".to_string()
                            } else {
                                "入力モード: 挿入 (クリックまたはInsertキーで上書きに切替)".to_string()
                            }
                        }
                        aria-label=move || {
                            let _ = shell.stats.get();
                            let is_overwrite = editor::is_focused_overwrite_mode();
                            let enabled = crate::settings::current().enable_overwrite_mode;
                            if !enabled && !is_overwrite {
                                "入力モード: 挿入 (上書きモードは設定で無効化されています)"
                            } else if is_overwrite {
                                "入力モード: 上書き (クリックまたはInsertキーで挿入に切替)"
                            } else {
                                "入力モード: 挿入 (クリックまたはInsertキーで上書きに切替)"
                            }
                        }
                        on:click=move |_| {
                            if let Some(focused) = editor::session() {
                                if crate::settings::current().enable_overwrite_mode || editor::is_focused_overwrite_mode() {
                                    editor::toggle_overwrite_mode(&focused);
                                    shell.stats.update(|_| {});
                                }
                            }
                        }
                    >
                        {move || {
                            let _ = shell.stats.get();
                            if editor::is_focused_overwrite_mode() {
                                "上書き"
                            } else {
                                "挿入"
                            }
                        }}
                    </button>
                    <span>{move || {
                        let stats = shell.stats.get();
                        format!("[ {} | {} ]", stats.caret_line, stats.caret_col)
                    }}</span>
                    <span
                        class="status-clickable"
                        title="構文モード（言語）を変更"
                        on:click=move |ev: web_sys::MouseEvent| {
                            let x = ev.client_x() as f64;
                            let y = ev.client_y() as f64;
                            language_menu.set(Some((x, y)));
                            encoding_menu.set(None);
                            line_ending_menu.set(None);
                        }
                    >
                        {move || shell.tab_language_name()}
                    </span>
                    <span
                        class="status-clickable"
                        title="文字コードを変更 / 開き直す"
                        on:click=move |ev: web_sys::MouseEvent| {
                            let x = ev.client_x() as f64;
                            let y = ev.client_y() as f64;
                            encoding_menu.set(Some((x, y)));
                            line_ending_menu.set(None);
                            language_menu.set(None);
                        }
                    >
                        {move || shell.tab().encoding.get()}
                    </span>
                    <span
                        class="status-clickable"
                        title="改行コードを変更"
                        on:click=move |ev: web_sys::MouseEvent| {
                            let x = ev.client_x() as f64;
                            let y = ev.client_y() as f64;
                            line_ending_menu.set(Some((x, y)));
                            encoding_menu.set(None);
                            language_menu.set(None);
                        }
                    >
                        {move || shell.tab().line_ending.get()}
                    </span>
                </div>
            </div>

            <Show when=move || encoding_menu.get().is_some()>
                {move || {
                    let (x, y) = encoding_menu.get()?;
                    let encodings = [
                        ("UTF-8", "UTF-8"),
                        ("UTF-8 (BOM)", "UTF-8 (BOM)"),
                        ("Shift-JIS", "Shift-JIS (CP932)"),
                        ("EUC-JP", "EUC-JP"),
                        ("ISO-2022-JP", "ISO-2022-JP (JIS)"),
                    ];
                    Some(view! {
                        <div
                            class="tab-context-menu-backdrop"
                            on:mousedown=move |_| encoding_menu.set(None)
                            on:contextmenu=move |ev| {
                                ev.prevent_default();
                                encoding_menu.set(None);
                            }
                        >
                            <div
                                class="tab-context-menu"
                                style=format!("left: {}px; bottom: {}px;", x.max(8.0), (web_sys::window().and_then(|w| w.inner_height().ok()).and_then(|h| h.as_f64()).unwrap_or(600.0) - y + 6.0).max(24.0))
                                on:mousedown=move |ev| ev.stop_propagation()
                            >
                                <div class="status-menu-header">"エンコードを指定して再読み込み"</div>
                                {encodings.iter().map(|&(enc_id, label)| {
                                    view! {
                                        <button
                                            class="context-menu-item"
                                            on:click=move |_| {
                                                shell.reopen_with_encoding(enc_id);
                                                encoding_menu.set(None);
                                            }
                                        >
                                            {label}
                                        </button>
                                    }
                                }).collect::<Vec<_>>()}
                                <div class="context-menu-separator"/>
                                <div class="status-menu-header">"エンコードを指定して保存"</div>
                                {encodings.iter().map(|&(enc_id, label)| {
                                    view! {
                                        <button
                                            class="context-menu-item"
                                            on:click=move |_| {
                                                shell.set_encoding(enc_id);
                                                encoding_menu.set(None);
                                            }
                                        >
                                            {label}
                                        </button>
                                    }
                                }).collect::<Vec<_>>()}
                            </div>
                        </div>
                    })
                }}
            </Show>

            <Show when=move || line_ending_menu.get().is_some()>
                {move || {
                    let (x, y) = line_ending_menu.get()?;
                    let line_endings = [
                        ("CRLF", "CRLF (Windows: \\r\\n)"),
                        ("LF", "LF (Unix/macOS: \\n)"),
                        ("CR", "CR (Classic Mac: \\r)"),
                    ];
                    Some(view! {
                        <div
                            class="tab-context-menu-backdrop"
                            on:mousedown=move |_| line_ending_menu.set(None)
                            on:contextmenu=move |ev| {
                                ev.prevent_default();
                                line_ending_menu.set(None);
                            }
                        >
                            <div
                                class="tab-context-menu"
                                style=format!("left: {}px; bottom: {}px;", x.max(8.0), (web_sys::window().and_then(|w| w.inner_height().ok()).and_then(|h| h.as_f64()).unwrap_or(600.0) - y + 6.0).max(24.0))
                                on:mousedown=move |ev| ev.stop_propagation()
                            >
                                <div class="status-menu-header">"改行コードを選択"</div>
                                {line_endings.iter().map(|&(le_id, label)| {
                                    view! {
                                        <button
                                            class="context-menu-item"
                                            on:click=move |_| {
                                                shell.set_line_ending(le_id);
                                                line_ending_menu.set(None);
                                            }
                                        >
                                            {label}
                                        </button>
                                    }
                                }).collect::<Vec<_>>()}
                            </div>
                        </div>
                    })
                }}
            </Show>

            <Show when=move || language_menu.get().is_some()>
                {move || {
                    let (x, y) = language_menu.get()?;
                    let languages = [
                        ("auto", "自動判定 (拡張子から)"),
                        ("Plain Text", "Plain Text"),
                        ("Markdown", "Markdown"),
                        ("Rust", "Rust"),
                        ("Kotlin", "Kotlin"),
                        ("TypeScript", "TypeScript"),
                        ("JavaScript", "JavaScript"),
                        ("Python", "Python"),
                        ("TOML", "TOML"),
                        ("JSON", "JSON"),
                        ("HTML", "HTML"),
                        ("CSS", "CSS"),
                        ("LaTeX", "LaTeX"),
                    ];
                    Some(view! {
                        <div
                            class="tab-context-menu-backdrop"
                            on:mousedown=move |_| language_menu.set(None)
                            on:contextmenu=move |ev| {
                                ev.prevent_default();
                                language_menu.set(None);
                            }
                        >
                            <div
                                class="tab-context-menu"
                                style=format!("left: {}px; bottom: {}px; max-height: 360px; overflow-y: auto;", x.max(8.0), (web_sys::window().and_then(|w| w.inner_height().ok()).and_then(|h| h.as_f64()).unwrap_or(600.0) - y + 6.0).max(24.0))
                                on:mousedown=move |ev| ev.stop_propagation()
                            >
                                <div class="status-menu-header">"構文モード（言語）を選択"</div>
                                {languages.iter().map(|&(lang_id, label)| {
                                    view! {
                                        <button
                                            class="context-menu-item"
                                            on:click=move |_| {
                                                if lang_id == "auto" {
                                                    shell.set_language(None);
                                                } else {
                                                    shell.set_language(Some(lang_id));
                                                }
                                                language_menu.set(None);
                                            }
                                        >
                                            {label}
                                        </button>
                                    }
                                }).collect::<Vec<_>>()}
                            </div>
                        </div>
                    })
                }}
            </Show>

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

fn format_file_size(bytes: usize) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1_000_000_000.0)
    } else if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    } else if bytes >= 1_000 {
        format!("{:.1} KB", bytes as f64 / 1_000.0)
    } else {
        format!("{bytes} B")
    }
}
