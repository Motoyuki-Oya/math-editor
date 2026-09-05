//! 検索と置換のバー。各ペインの右上にフローティング表示されます。

use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::JsCast;
use web_sys::KeyboardEvent;

use super::shell::{Field, Pane, Shell};
use crate::editor;
use crate::framework::{estimate_matches, file_size};

#[component]
pub fn FindBar(shell: Shell, pane: Pane) -> impl IntoView {
    let query_field: NodeRef<leptos::html::Input> = NodeRef::new();
    let replacement_field: NodeRef<leptos::html::Input> = NodeRef::new();
    let query = RwSignal::new(String::new());
    let replacement = RwSignal::new(String::new());
    let regex = RwSignal::new(false);
    let case_sensitive = RwSignal::new(false);
    let replace_expanded = RwSignal::new(false);
    let current_match = RwSignal::new(0usize);
    let estimated_count = RwSignal::new(None::<usize>);
    let estimate_generation = RwSignal::new(0u64);
    let options = move || editor::SearchOptions {
        regex: regex.get_untracked(),
        case_sensitive: case_sensitive.get_untracked(),
    };

    on_cleanup(move || {
        editor::clear_search_preview_pane(pane.editor_pane());
    });

    let last_matched_pos = RwSignal::new(None::<(usize, usize)>);

    // マッチインデックスを 1 ステップ動かす。
    // `forward`: true なら +1（次を検索）、false なら -1（前を検索）。
    let step_match = move |forward: bool| {
        shell.focus_on(pane);
        let epane = pane.editor_pane();
        let q = query.get_untracked();
        let (pane_cur, pane_total) = pane.search_status.get_untracked();
        let total = pane_total.or_else(|| estimated_count.get_untracked()).unwrap_or(0);
        let cur_cursor = editor::current_cursor_pos_pane(epane);
        let caret_moved = last_matched_pos.get_untracked() != cur_cursor || cur_cursor.is_none();

        let base = if pane_cur > 0 {
            pane_cur
        } else if caret_moved {
            editor::current_match_number_pane(epane, &q, options(), total)
        } else {
            current_match.get_untracked()
        };

        let next = if forward {
            if base >= total && total > 0 {
                1
            } else {
                base + 1
            }
        } else if base <= 1 {
            if total > 0 {
                total
            } else {
                1
            }
        } else {
            base - 1
        };
        current_match.set(next);
        pane.search_status.set((next, pane_total.or_else(|| estimated_count.get_untracked())));
    };

    // バーは開いた後のみ画面上に表示されるため、フィールドが存在するとすぐに、
    // カーソルは要求されたフィールドに置かれます。
    Effect::new(move |_| {
        let field = match shell.find_focus.get() {
            Some(Field::Query) => query_field.get(),
            Some(Field::Replacement) => {
                replace_expanded.set(true);
                replacement_field.get()
            }
            None => return,
        };
        if let Some(field) = field {
            field.focus().ok();
            field.select();
            shell.find_focus.set(None);
        }
    });

    // 検索ジャンプの後に呼ぶ: ジャンプ先をキャレット移動検知用に記録する。
    let record_pos = move || {
        last_matched_pos.set(editor::current_cursor_pos_pane(pane.editor_pane()));
    };

    // on:focus で現在位置を同期する。
    let sync_on_focus = move || {
        shell.focus_on(pane);
        let epane = pane.editor_pane();
        let q = query.get_untracked();
        let (pane_cur, pane_total) = pane.search_status.get_untracked();
        let total = pane_total.or_else(|| estimated_count.get_untracked()).unwrap_or(0);
        let num = if pane_cur > 0 {
            pane_cur
        } else {
            editor::current_match_number_pane(epane, &q, options(), total)
        };
        current_match.set(num);
        last_matched_pos.set(editor::current_cursor_pos_pane(epane));
    };

    let close_bar = move || {
        editor::clear_search_preview_pane(pane.editor_pane());
        pane.searching.set(false);
        shell.searching.set(false);
        editor::focus_pane(pane.editor_pane());
    };

    view! {
        <div class="findbar" on:mousedown=move |ev| ev.stop_propagation()>
            <div class="findbar-row">
                <button
                    class="find-toggle-replace"
                    title=move || if replace_expanded.get() { "置換を閉じる" } else { "置換を開く" }
                    on:click=move |_| replace_expanded.update(|open| *open = !*open)
                >
                    {move || if replace_expanded.get() { "▾" } else { "▸" }}
                </button>
                <div class="find-input-wrap">
                    <input
                        class="find-input"
                        node_ref=query_field
                        placeholder="検索"
                        prop:value=move || query.get()
                        on:focus=move |_| sync_on_focus()
                        on:input=move |ev| {
                            let value = event_target_value(&ev);
                            query.set(value.clone());
                            current_match.set(0);
                            pane.search_status.set((0, None));
                            last_matched_pos.set(None);
                            update_preview(
                                pane,
                                value,
                                options(),
                                estimated_count,
                                estimate_generation,
                            );
                        }
                        on:keydown=move |ev: KeyboardEvent| {
                            if ev.key() == "Escape" {
                                ev.prevent_default();
                                close_bar();
                                return;
                            }
                            if ev.key() == "Enter" && !ev.is_composing() {
                                ev.prevent_default();
                                let shell = shell;
                                let query = query.get_untracked();
                                let options = options();
                                let forward = !ev.shift_key();
                                step_match(forward);
                                if forward {
                                    super::sync::find(shell, pane.editor_pane(), query, options, None);
                                } else {
                                    super::sync::find_previous(shell, pane.editor_pane(), query, options, None);
                                }
                                record_pos();
                            }
                        }
                    />
                    <span class="find-count-badge">
                        {move || {
                            let q = query.get();
                            if q.is_empty() {
                                return "".to_string();
                            }
                            let (pane_cur, pane_total) = pane.search_status.get();
                            let cur = if pane_cur > 0 {
                                pane_cur
                            } else {
                                current_match.get()
                            };
                            let total = pane_total.or_else(|| estimated_count.get());
                            match total {
                                Some(0) => "0/0".to_string(),
                                Some(t) => format!("{cur}/{t}"),
                                None => format!("{cur}/…"),
                            }
                        }}
                    </span>
                    <div class="find-options-inline">
                        <button
                            class=move || if case_sensitive.get() { "find-opt-btn active" } else { "find-opt-btn" }
                            title="大文字・小文字を区別 (Aa)"
                            on:click=move |_| {
                                case_sensitive.update(|v| *v = !*v);
                                update_preview(
                                    pane,
                                    query.get_untracked(),
                                    options(),
                                    estimated_count,
                                    estimate_generation,
                                );
                            }
                        >
                            "Aa"
                        </button>
                        <button
                            class=move || if regex.get() { "find-opt-btn active" } else { "find-opt-btn" }
                            title="正規表現 (.*)"
                            on:click=move |_| {
                                regex.update(|v| *v = !*v);
                                update_preview(
                                    pane,
                                    query.get_untracked(),
                                    options(),
                                    estimated_count,
                                    estimate_generation,
                                );
                            }
                        >
                            ".*"
                        </button>
                    </div>
                </div>
                <div class="find-actions">
                    <button
                        class="find-icon-btn"
                        title="前を検索 (Shift+Enter)"
                        on:click=move |_| {
                            let shell = shell;
                            let query = query.get_untracked();
                            let options = options();
                            step_match(false);
                            super::sync::find_previous(shell, pane.editor_pane(), query, options, None);
                            record_pos();
                        }
                    >
                        "↑"
                    </button>
                    <button
                        class="find-icon-btn"
                        title="次を検索 (Enter)"
                        on:click=move |_| {
                            let shell = shell;
                            let query = query.get_untracked();
                            let options = options();
                            step_match(true);
                            super::sync::find(shell, pane.editor_pane(), query, options, None);
                            record_pos();
                        }
                    >
                        "↓"
                    </button>
                    <button
                        class="find-icon-btn find-close-btn"
                        title="閉じる (Escape)"
                        on:click=move |_| close_bar()
                    >
                        "✕"
                    </button>
                </div>
            </div>

            <Show when=move || replace_expanded.get()>
                <div class="findbar-row findbar-replace-row">
                    <div class="find-toggle-spacer"/>
                    <div class="find-input-wrap">
                        <input
                            class="find-input"
                            node_ref=replacement_field
                            placeholder="置換後"
                            prop:value=move || replacement.get()
                            on:input=move |ev| replacement.set(event_target_value(&ev))
                            on:keydown=move |ev: KeyboardEvent| {
                                if ev.key() == "Escape" {
                                    ev.prevent_default();
                                    close_bar();
                                    return;
                                }
                                if ev.key() == "Enter" && !ev.is_composing() {
                                    ev.prevent_default();
                                    let query = query.get_untracked();
                                    let replacement = replacement.get_untracked();
                                    let options = options();
                                    step_match(true);
                                    spawn_local(async move {
                                        let size = file_size_for(pane).await;
                                        editor::replace_and_find_next_pane(
                                            pane.editor_pane(),
                                            &query,
                                            &replacement,
                                            options,
                                            size,
                                        );
                                        record_pos();
                                    });
                                }
                            }
                        />
                    </div>
                    <div class="find-actions">
                        <button
                            class="find-icon-btn"
                            title="置換して次へ"
                            on:click=move |_| {
                                let query = query.get_untracked();
                                let replacement = replacement.get_untracked();
                                let options = options();
                                step_match(true);
                                spawn_local(async move {
                                    let size = file_size_for(pane).await;
                                    editor::replace_and_find_next_pane(
                                        pane.editor_pane(),
                                        &query,
                                        &replacement,
                                        options,
                                        size,
                                    );
                                    record_pos();
                                });
                            }
                        >
                            "ab→ac"
                        </button>
                        <button
                            class="find-icon-btn"
                            title="すべて置換"
                            on:click=move |_| {
                                let shell = shell;
                                let query = query.get_untracked();
                                let replacement = replacement.get_untracked();
                                let options = options();
                                spawn_local(async move {
                                    if !editor::fully_resident() {
                                        shell.status.set(
                                            "大きなファイルのすべて置換はまだ対応していません".into(),
                                        );
                                        return;
                                    }
                                    let size = file_size_for(pane).await;
                                    let replaced = editor::replace_all(&query, &replacement, options, size);
                                    shell.status.set(format!("{replaced} 件置換しました"));
                                });
                            }
                        >
                            "ab⇒ac"
                        </button>
                    </div>
                </div>
            </Show>
        </div>
    }
}

fn update_preview(
    pane: Pane,
    query: String,
    options: editor::SearchOptions,
    estimated_count: RwSignal<Option<usize>>,
    generation: RwSignal<u64>,
) {
    editor::preview_search_pane(pane.editor_pane(), &query, options);
    estimated_count.set(None);
    let current = generation.get_untracked() + 1;
    generation.set(current);
    if query.is_empty() {
        return;
    }
    spawn_local(async move {
        tick(200).await;
        if generation.get_untracked() != current {
            return;
        }
        let Some(handle) = pane.tab_untracked().doc.get_untracked() else {
            return;
        };
        let result = estimate_matches(handle, &query, options.regex, options.case_sensitive).await;
        if generation.get_untracked() == current {
            if let Ok(count) = result {
                estimated_count.set(Some(count));
            }
        } else {
            return;
        }

        // バックグラウンドで全文並列走査を実行し、確定総数へ随時更新する。
        let page = crate::framework::search_document(
            handle,
            &query,
            options.regex,
            options.case_sensitive,
            crate::format::document::NOTATION_MARK,
            0,
            0,
            None,
            true,
        )
        .await;
        if generation.get_untracked() == current {
            if let Ok(page) = page {
                if !page.cancelled {
                    if let Some(total) = page.total_matches {
                        estimated_count.set(Some(total));
                    }
                }
            }
        }
    });
}

async fn tick(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        if let Some(window) = web_sys::window() {
            window
                .set_timeout_with_callback_and_timeout_and_arguments_0(resolve.unchecked_ref(), ms)
                .ok();
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

async fn file_size_for(pane: Pane) -> Option<usize> {
    let tab = pane.tab_untracked();
    let path = tab.path.get_untracked();
    file_size(path.as_deref(), tab.id.get_untracked()).await
}
