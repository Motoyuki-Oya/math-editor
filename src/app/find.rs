//! 検索と置換のバー。

use leptos::prelude::*;
use leptos::task::spawn_local;
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
    let options = move || editor::SearchOptions {
        regex: regex.get_untracked(),
        case_sensitive: case_sensitive.get_untracked(),
    };

    // バーは開いた後のみ画面上に表示されるため、フィールドが存在するとすぐに、カーソルは要求されたフィールドに置かれます。
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

    view! {
                <div class="findbar">
                    <input
                        class="find-input"
                        node_ref=query_field
                        placeholder="検索"
                        prop:value=move || query.get()
                        on:input=move |ev| query.set(event_target_value(&ev))
                        on:keydown=move |ev: KeyboardEvent| {
                            if ev.key() == "Enter" {
                                let shell = shell;
                                let query = query.get_untracked();
                                let options = options();
                                spawn_local(async move {
                                    let size = file_size_for(shell).await;
                                    super::sync::find(shell, query, options, size);
                                });
                            }
                        }
                    />
                    <button class="tool" on:click=move |_| {
                        let shell = shell;
                        let query = query.get_untracked();
                        let options = options();
                        spawn_local(async move {
                            let size = file_size_for(shell).await;
                            super::sync::find(shell, query, options, size);
                        });
                    }>"次を検索"</button>
                    <input
                        class="find-input"
                        node_ref=replacement_field
                        placeholder="置換後"
                        prop:value=move || replacement.get()
                        on:input=move |ev| replacement.set(event_target_value(&ev))
                    />
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
                            on:change=move |ev| case_sensitive.set(event_target_checked(&ev))
                        />
                        "Aa"
                    </label>
                    <label class="find-toggle" title="正規表現（置換では $1 で後方参照、\\t で列の区切り、\\n で改行）">
                        <input
                            type="checkbox"
                            prop:checked=move || regex.get()
                            on:change=move |ev| regex.set(event_target_checked(&ev))
                        />
                        ".*"
                    </label>
                    <button class="tool" on:click=move |_| shell.searching.set(false)>"閉じる"</button>
                </div>
    }
}

async fn file_size_for(shell: Shell) -> Option<usize> {
    let tab = shell.tab_untracked();
    let path = tab.path.get_untracked();
    ipc::file_size(path.as_deref(), tab.id.get_untracked()).await
}
