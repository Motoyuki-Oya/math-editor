//! 設定バー。ライターが実際に変更する内容、つまりテキストのサイズと文字面、およびキャレットが点滅するかどうかのみが表示されます。 `crate::settings` の他のすべては、邪魔にならないようにファイル内に残ります。

use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::JsCast;

use crate::editor;
use crate::framework::write_settings;
use crate::settings;
use crate::settings::Settings;

/// 設定を画面に効かせるただ 1 つの入口。設定の一部（行番号や折り返し）は行を描くときに行へ入るので、適用と描き直しは常に一緒です。ここを通らない適用は、画面と設定が食い違ったままになります。
pub(super) fn take_effect(settings: Settings) {
    settings::apply(settings);
    editor::redraw_all();
    super::menu::update_menu_state();
}

/// ユーザーが設定を変えたとき: 効かせて、次回起動用のファイルにも書きます。
pub(super) fn change(settings: Settings) {
    take_effect(settings.clone());
    spawn_local(async move {
        write_settings(&settings::write(&settings)).await;
    });
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Category {
    General,
    Appearance,
    Editor,
}

#[component]
pub(super) fn Preferences(open: RwSignal<bool>) -> impl IntoView {
    let current = settings::current();
    let selected_category = RwSignal::new(Category::General);

    let font_size = RwSignal::new(current.font_size);
    let font_family = RwSignal::new(current.font_family);
    let caret_blink = RwSignal::new(current.caret_blink);
    let wrap = RwSignal::new(current.wrap);
    let line_numbers = RwSignal::new(current.line_numbers);
    let show_whitespace = RwSignal::new(current.show_whitespace);
    let column_gap = RwSignal::new(current.column_gap);
    let history_limit = RwSignal::new(current.history_limit);
    let global_shortcut = RwSignal::new(current.global_shortcut);
    let font_ligatures = RwSignal::new(current.font_ligatures);
    let enable_overwrite_mode = RwSignal::new(current.enable_overwrite_mode);

    let changed = move || {
        change(Settings {
            font_size: font_size.get_untracked(),
            font_family: font_family.get_untracked().trim().to_string(),
            caret_blink: caret_blink.get_untracked(),
            wrap: wrap.get_untracked(),
            line_numbers: line_numbers.get_untracked(),
            column_gap: column_gap.get_untracked(),
            history_limit: history_limit.get_untracked(),
            global_shortcut: global_shortcut.get_untracked(),
            show_whitespace: show_whitespace.get_untracked(),
            font_ligatures: font_ligatures.get_untracked(),
            enable_overwrite_mode: enable_overwrite_mode.get_untracked(),
        });
    };

    let reset_defaults = move |_| {
        let defaults = Settings::default();
        font_size.set(defaults.font_size);
        font_family.set(defaults.font_family.clone());
        caret_blink.set(defaults.caret_blink);
        wrap.set(defaults.wrap);
        line_numbers.set(defaults.line_numbers);
        column_gap.set(defaults.column_gap);
        history_limit.set(defaults.history_limit);
        global_shortcut.set(defaults.global_shortcut);
        show_whitespace.set(defaults.show_whitespace);
        font_ligatures.set(defaults.font_ligatures);
        enable_overwrite_mode.set(defaults.enable_overwrite_mode);
        change(defaults);
    };

    let close_dialog = move || open.set(false);

    // Escape キーで閉じる
    let on_keydown = move |ev: web_sys::KeyboardEvent| {
        if ev.key() == "Escape" {
            ev.prevent_default();
            close_dialog();
        }
    };

    view! {
        <div
            class="modal-backdrop"
            tabindex="-1"
            on:keydown=on_keydown
            on:mousedown=move |ev: web_sys::MouseEvent| {
                if let Some(target) = ev.target() {
                    if let Ok(el) = target.dyn_into::<web_sys::HtmlElement>() {
                        if el.class_list().contains("modal-backdrop") {
                            close_dialog();
                        }
                    }
                }
            }
        >
            <div class="settings-dialog" on:mousedown=move |ev| ev.stop_propagation()>
                // 左サイドバー
                <div class="settings-sidebar">
                    <div class="settings-sidebar-header">
                        <button
                            class="settings-close-btn"
                            title="閉じる (Escape)"
                            on:click=move |_| close_dialog()
                        >
                            "✕"
                        </button>
                        <span class="settings-sidebar-title">"設定"</span>
                    </div>

                    <nav class="settings-nav">
                        <button
                            class=move || if selected_category.get() == Category::General {
                                "settings-nav-item active"
                            } else {
                                "settings-nav-item"
                            }
                            on:click=move |_| selected_category.set(Category::General)
                        >
                            <span class="settings-nav-icon">"⚙"</span>
                            <span class="settings-nav-label">"一般"</span>
                        </button>
                        <button
                            class=move || if selected_category.get() == Category::Appearance {
                                "settings-nav-item active"
                            } else {
                                "settings-nav-item"
                            }
                            on:click=move |_| selected_category.set(Category::Appearance)
                        >
                            <span class="settings-nav-icon">"🔤"</span>
                            <span class="settings-nav-label">"フォント・表示"</span>
                        </button>
                        <button
                            class=move || if selected_category.get() == Category::Editor {
                                "settings-nav-item active"
                            } else {
                                "settings-nav-item"
                            }
                            on:click=move |_| selected_category.set(Category::Editor)
                        >
                            <span class="settings-nav-icon">"📐"</span>
                            <span class="settings-nav-label">"エディタ・構造"</span>
                        </button>
                    </nav>

                    <div class="settings-sidebar-footer">
                        <button class="settings-reset-btn" on:click=reset_defaults>
                            "既定に戻す"
                        </button>
                    </div>
                </div>

                // 右コンテンツエリア
                <div class="settings-content">
                    {move || match selected_category.get() {
                        Category::General => view! {
                            <div class="settings-panel">
                                <h2 class="settings-panel-title">"一般"</h2>

                                <div class="settings-item">
                                    <div class="settings-item-info">
                                        <div class="settings-item-name">"右端で折り返す"</div>
                                        <div class="settings-item-desc">"ウィンドウ幅を超える長い行を自動で次の行に折り返して表示します。"</div>
                                    </div>
                                    <label class="switch">
                                        <input
                                            type="checkbox"
                                            prop:checked=move || wrap.get()
                                            on:change=move |ev| {
                                                wrap.set(event_target_checked(&ev));
                                                changed();
                                            }
                                        />
                                        <span class="slider"/>
                                    </label>
                                </div>

                                <div class="settings-item">
                                    <div class="settings-item-info">
                                        <div class="settings-item-name">"行番号を表示する"</div>
                                        <div class="settings-item-desc">"エディタの左側に行番号を表示します。"</div>
                                    </div>
                                    <label class="switch">
                                        <input
                                            type="checkbox"
                                            prop:checked=move || line_numbers.get()
                                            on:change=move |ev| {
                                                line_numbers.set(event_target_checked(&ev));
                                                changed();
                                            }
                                        />
                                        <span class="slider"/>
                                    </label>
                                </div>

                                <div class="settings-item">
                                    <div class="settings-item-info">
                                        <div class="settings-item-name">"空白文字を表示する"</div>
                                        <div class="settings-item-desc">"半角スペース、全角スペース、タブなどの記号を可視化します。"</div>
                                    </div>
                                    <label class="switch">
                                        <input
                                            type="checkbox"
                                            prop:checked=move || show_whitespace.get()
                                            on:change=move |ev| {
                                                show_whitespace.set(event_target_checked(&ev));
                                                changed();
                                            }
                                        />
                                        <span class="slider"/>
                                    </label>
                                </div>

                                <div class="settings-item">
                                    <div class="settings-item-info">
                                        <div class="settings-item-name">"カーソルの点滅"</div>
                                        <div class="settings-item-desc">"キャレットを一定間隔で点滅させます。"</div>
                                    </div>
                                    <label class="switch">
                                        <input
                                            type="checkbox"
                                            prop:checked=move || caret_blink.get()
                                            on:change=move |ev| {
                                                caret_blink.set(event_target_checked(&ev));
                                                changed();
                                            }
                                        />
                                        <span class="slider"/>
                                    </label>
                                </div>

                                <div class="settings-item">
                                    <div class="settings-item-info">
                                        <div class="settings-item-name">"常駐ショートカット"</div>
                                        <div class="settings-item-desc">"Ctrl+Alt+M でいつでもアプリを最前面に呼び出します。"</div>
                                    </div>
                                    <label class="switch">
                                        <input
                                            type="checkbox"
                                            prop:checked=move || global_shortcut.get()
                                            on:change=move |ev| {
                                                global_shortcut.set(event_target_checked(&ev));
                                                changed();
                                            }
                                        />
                                        <span class="slider"/>
                                    </label>
                                </div>
                            </div>
                        }.into_any(),

                        Category::Appearance => view! {
                            <div class="settings-panel">
                                <h2 class="settings-panel-title">"フォント・表示"</h2>

                                <div class="settings-item">
                                    <div class="settings-item-info">
                                        <div class="settings-item-name">"文字の大きさ"</div>
                                        <div class="settings-item-desc">"エディタの標準フォントサイズ（ピクセル単位）を設定します。"</div>
                                    </div>
                                    <div class="settings-control">
                                        <input
                                            class="settings-input-num"
                                            type="number"
                                            min="8"
                                            max="48"
                                            prop:value=move || font_size.get().to_string()
                                            on:change=move |ev| {
                                                if let Ok(size) = event_target_value(&ev).parse::<f64>() {
                                                    font_size.set(size.clamp(8.0, 48.0));
                                                    changed();
                                                }
                                            }
                                        />
                                        <span class="settings-unit">"px"</span>
                                    </div>
                                </div>

                                <div class="settings-item">
                                    <div class="settings-item-info">
                                        <div class="settings-item-name">"字体（フォントファミリー）"</div>
                                        <div class="settings-item-desc">"テキストと構造を描画するフォントを指定します。"</div>
                                    </div>
                                    <div class="settings-control">
                                        <select
                                            class="settings-select"
                                            prop:value=move || font_family.get()
                                            on:change=move |ev| {
                                                let new_font = event_target_value(&ev);
                                                if let Some(url) = crate::font_loader::get_download_url(&new_font) {
                                                    if !crate::font_loader::is_font_cached_or_local(&new_font) {
                                                        let msg = format!(
                                                            "フォント「{}」がローカルに保存されていません。\nインターネットからダウンロードしてローカルに保存しますか？",
                                                            new_font
                                                        );
                                                        if let Some(window) = web_sys::window() {
                                                            let confirmed = window.confirm_with_message(&msg).unwrap_or(false);
                                                            if confirmed {
                                                                let f_name = new_font.clone();
                                                                let u = url.to_string();
                                                                font_family.set(new_font);
                                                                changed();
                                                                spawn_local(async move {
                                                                    if let Ok(()) = crate::font_loader::download_and_save_font(&f_name, &u).await {
                                                                        editor::redraw_all();
                                                                    }
                                                                });
                                                                return;
                                                            } else {
                                                                let prev = font_family.get_untracked();
                                                                font_family.set(prev);
                                                                return;
                                                            }
                                                        }
                                                    }
                                                }
                                                font_family.set(new_font);
                                                changed();
                                            }
                                        >
                                            <option value="">"既定 (Segoe UI / 可変幅)"</option>
                                            <option value="Cascadia Code">"Cascadia Code [合字対応]"</option>
                                            <option value="Fira Code">"Fira Code [合字対応]"</option>
                                            <option value="JetBrains Mono">"JetBrains Mono [合字対応]"</option>
                                            <option value="Consolas">"Consolas (等幅・狭め)"</option>
                                            <option value="Source Code Pro">"Source Code Pro (等幅)"</option>
                                            <option value="BIZ UDゴシック">"BIZ UDゴシック (等幅)"</option>
                                            <option value="Meiryo">"Meiryo (可変幅)"</option>
                                            <option value="Segoe UI">"Segoe UI (可変幅)"</option>
                                        </select>
                                    </div>
                                </div>

                                <div class="settings-item">
                                    <div class="settings-item-info">
                                        <div class="settings-item-name">"合字（リガチャー）"</div>
                                        <div class="settings-item-desc">"対応フォントで -> や != 等の記号を一体のグリフとして描画します。"</div>
                                    </div>
                                    <label class="switch">
                                        <input
                                            type="checkbox"
                                            prop:checked=move || font_ligatures.get()
                                            on:change=move |ev| {
                                                font_ligatures.set(event_target_checked(&ev));
                                                changed();
                                            }
                                        />
                                        <span class="slider"/>
                                    </label>
                                </div>
                            </div>
                        }.into_any(),

                        Category::Editor => view! {
                            <div class="settings-panel">
                                <h2 class="settings-panel-title">"エディタ・構造"</h2>

                                <div class="settings-item">
                                    <div class="settings-item-info">
                                        <div class="settings-item-name">"列の間隔 (Column gap)"</div>
                                        <div class="settings-item-desc">"整列されたタブ列間の横スペース（ピクセル単位）を設定します。"</div>
                                    </div>
                                    <div class="settings-control">
                                        <input
                                            class="settings-input-num"
                                            type="number"
                                            min="4"
                                            max="64"
                                            prop:value=move || column_gap.get().to_string()
                                            on:change=move |ev| {
                                                if let Ok(gap) = event_target_value(&ev).parse::<f64>() {
                                                    column_gap.set(gap.clamp(4.0, 64.0));
                                                    changed();
                                                }
                                            }
                                        />
                                        <span class="settings-unit">"px"</span>
                                    </div>
                                </div>

                                <div class="settings-item">
                                    <div class="settings-item-info">
                                        <div class="settings-item-name">"履歴保持件数 (History limit)"</div>
                                        <div class="settings-item-desc">"Undo / Redo 用に保持する最大ステップ数を設定します。"</div>
                                    </div>
                                    <div class="settings-control">
                                        <input
                                            class="settings-input-num"
                                            type="number"
                                            min="50"
                                            max="5000"
                                            step="50"
                                            prop:value=move || history_limit.get().to_string()
                                            on:change=move |ev| {
                                                if let Ok(limit) = event_target_value(&ev).parse::<usize>() {
                                                    history_limit.set(limit.clamp(50, 5000));
                                                    changed();
                                                }
                                            }
                                        />
                                        <span class="settings-unit">"件"</span>
                                    </div>
                                </div>

                                <div class="settings-item">
                                    <div class="settings-item-info">
                                        <div class="settings-item-name">"上書き入力モード (Insertキー)"</div>
                                        <div class="settings-item-desc">"Insertキーによる挿入／上書き入力モードの切り替えを有効にします。"</div>
                                    </div>
                                    <label class="switch">
                                        <input
                                            type="checkbox"
                                            prop:checked=move || enable_overwrite_mode.get()
                                            on:change=move |ev| {
                                                enable_overwrite_mode.set(event_target_checked(&ev));
                                                changed();
                                            }
                                        />
                                        <span class="slider"/>
                                    </label>
                                </div>
                            </div>
                        }.into_any(),
                    }}
                </div>
            </div>
        </div>
    }
}
