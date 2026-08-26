//! 検索と置換のバー。

use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::JsCast;
use web_sys::KeyboardEvent;

use super::shell::{Field, Shell};
use crate::editor;
use crate::ipc;

#[component]
pub fn FindBar(shell: Shell) -> impl IntoView {
    let query_field: NodeRef<leptos::html::Input> = NodeRef::new();
    let replacement_field: NodeRef<leptos::html::Input> = NodeRef::new();
    let query = RwSignal::new(String::new());
    let replacement = RwSignal::new(String::new());
    let regex = RwSignal::new(false);
    let case_sensitive = RwSignal::new(false);
    let current_match = RwSignal::new(0usize);
    let estimated_count = RwSignal::new(None::<usize>);
    let estimate_generation = RwSignal::new(0u64);
    let options = move || editor::SearchOptions {
        regex: regex.get_untracked(),
        case_sensitive: case_sensitive.get_untracked(),
    };

    on_cleanup(move || {
        editor::clear_search_preview();
    });

    let last_matched_pos = RwSignal::new(None::<(usize, usize)>);

    // マッチインデックスを 1 ステップ動かす。
    // `forward`: true なら +1（次を検索）、false なら -1（前を検索）。
    //
    // - キャレットが前回のジャンプ位置のまま → 単純にインクリメント/デクリメント
    // - キャレットが手動で移動された → 現在位置から何番目かを再計算
    let step_match = move |forward: bool| {
        let q = query.get_untracked();
        let total = estimated_count.get_untracked().unwrap_or(0);
        let cur_cursor = editor::current_cursor_pos();
        let caret_moved = last_matched_pos.get_untracked() != cur_cursor || cur_cursor.is_none();

        let base = if caret_moved {
            editor::current_match_number(&q, options(), total)
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
    };

    // バーは開いた後のみ画面上に表示されるため、フィールドが存在するとすぐに、
    // カーソルは要求されたフィールドに置かれます。
    Effect::new(move |_| {
        let field = match shell.find_focus.get() {
            Some(Field::Query) => query_field.get(),
            Some(Field::Replacement) => replacement_field.get(),
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
        last_matched_pos.set(editor::current_cursor_pos());
    };

    // on:focus で現在位置を同期する。
    let sync_on_focus = move || {
        let q = query.get_untracked();
        let total = estimated_count.get_untracked().unwrap_or(0);
        current_match.set(editor::current_match_number(&q, options(), total));
        last_matched_pos.set(editor::current_cursor_pos());
    };

    view! {
                <div class="findbar">
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
                            last_matched_pos.set(None);
                            update_preview(
                                shell,
                                value,
                                options(),
                                estimated_count,
                                estimate_generation,
                            );
                        }
                        on:keydown=move |ev: KeyboardEvent| {
                            if ev.key() == "Enter" && !ev.is_composing() {
                                ev.prevent_default();
                                let shell = shell;
                                let query = query.get_untracked();
                                let options = options();
                                let forward = !ev.shift_key();
                                step_match(forward);
                                spawn_local(async move {
                                    let size = file_size_for(shell).await;
                                    if forward {
                                        super::sync::find(shell, query, options, size);
                                    } else {
                                        super::sync::find_previous(shell, query, options, size);
                                    }
                                    record_pos();
                                });
                            }
                        }
                    />
                    <button class="tool" title="前を検索 (Shift+Enter)" on:click=move |_| {
                        let shell = shell;
                        let query = query.get_untracked();
                        let options = options();
                        step_match(false);
                        spawn_local(async move {
                            let size = file_size_for(shell).await;
                            super::sync::find_previous(shell, query, options, size);
                            record_pos();
                        });
                    }>"前を検索"</button>
                    <button class="tool" title="次を検索 (Enter)" on:click=move |_| {
                        let shell = shell;
                        let query = query.get_untracked();
                        let options = options();
                        step_match(true);
                        spawn_local(async move {
                            let size = file_size_for(shell).await;
                            super::sync::find(shell, query, options, size);
                            record_pos();
                        });
                    }>"次を検索"</button>
                    <span class="find-count">{move || {
                        let q = query.get();
                        if q.is_empty() {
                            return "0 / 0 件".to_string();
                        }
                        let cur = current_match.get();
                        match estimated_count.get() {
                            Some(0) => "0 / 0 件".to_string(),
                            Some(estimated) => format!("{cur} / {estimated} 件"),
                            None => format!("{cur} / 推定中…"),
                        }
                    }}</span>
                    <input
                        class="find-input"
                        node_ref=replacement_field
                        placeholder="置換後"
                        prop:value=move || replacement.get()
                        on:input=move |ev| replacement.set(event_target_value(&ev))
                        on:keydown=move |ev: KeyboardEvent| {
                            if ev.key() == "Enter" && !ev.is_composing() {
                                ev.prevent_default();
                                let shell = shell;
                                let query = query.get_untracked();
                                let replacement = replacement.get_untracked();
                                let options = options();
                                step_match(true);
                                spawn_local(async move {
                                    let size = file_size_for(shell).await;
                                    editor::replace_and_find_next(&query, &replacement, options, size);
                                    record_pos();
                                });
                            }
                        }
                    />
                    <button class="tool" on:click=move |_| {
                        let shell = shell;
                        let query = query.get_untracked();
                        let replacement = replacement.get_untracked();
                        let options = options();
                        step_match(true);
                        spawn_local(async move {
                            let size = file_size_for(shell).await;
                            editor::replace_and_find_next(&query, &replacement, options, size);
                            record_pos();
                        });
                    }>"置換して次へ"</button>
                    <button class="tool" on:click=move |_| {
                        let shell = shell;
                        let query = query.get_untracked();
                        let replacement = replacement.get_untracked();
                        let options = options();
                        spawn_local(async move {
                            // 手元に全部ない文書の置換は、まだ取得済みの行しか見ない。
                            // 黙って一部だけ置換するより、正直に断る。
                            if !editor::fully_resident() {
                                shell.status.set(
                                    "大きなファイルのすべて置換はまだ対応していません".into(),
                                );
                                return;
                            }
                            let size = file_size_for(shell).await;
                            let replaced = editor::replace_all(&query, &replacement, options, size);
                            shell.status.set(format!("{replaced} 件置換しました"));
                        });
                    }>"すべて置換"</button>
                    <label class="find-toggle" title="大文字小文字を区別">
                        <input
                            type="checkbox"
                            prop:checked=move || case_sensitive.get()
                            on:change=move |ev| {
                                case_sensitive.set(event_target_checked(&ev));
                                update_preview(
                                    shell,
                                    query.get_untracked(),
                                    options(),
                                    estimated_count,
                                    estimate_generation,
                                );
                            }
                        />
                        "Aa"
                    </label>
                    <label class="find-toggle" title="正規表現（置換では $1 で後方参照、\\t で列の区切り、\\n で改行）">
                        <input
                            type="checkbox"
                            prop:checked=move || regex.get()
                            on:change=move |ev| {
                                regex.set(event_target_checked(&ev));
                                update_preview(
                                    shell,
                                    query.get_untracked(),
                                    options(),
                                    estimated_count,
                                    estimate_generation,
                                );
                            }
                        />
                        ".*"
                    </label>
                    <button class="tool" on:click=move |_| {
                        editor::clear_search_preview();
                        shell.searching.set(false);
                    }>"閉じる"</button>
                </div>
    }
}

fn update_preview(
    shell: Shell,
    query: String,
    options: editor::SearchOptions,
    estimated_count: RwSignal<Option<usize>>,
    generation: RwSignal<u64>,
) {
    editor::preview_search(&query, options);
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
        let Some(handle) = shell.tab_untracked().doc.get_untracked() else {
            return;
        };
        let result =
            ipc::estimate_matches(handle, &query, options.regex, options.case_sensitive).await;
        if generation.get_untracked() == current {
            if let Ok(count) = result {
                estimated_count.set(Some(count));
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

async fn file_size_for(shell: Shell) -> Option<usize> {
    let tab = shell.tab_untracked();
    let path = tab.path.get_untracked();
    ipc::file_size(path.as_deref(), tab.id.get_untracked()).await
}
