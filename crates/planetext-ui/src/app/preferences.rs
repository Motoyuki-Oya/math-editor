//! 設定バー。ライターが実際に変更する内容、つまりテキストのサイズと文字面、およびキャレットが点滅するかどうかのみが表示されます。 `crate::settings` の他のすべては、邪魔にならないようにファイル内に残ります。

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::editor;
use crate::framework::write_settings;
use crate::settings;
use crate::settings::Settings;

/// 設定を画面に効かせるただ 1 つの入口。設定の一部（行番号や折り返し）は行を描くときに行へ入るので、適用と描き直しは常に一緒です。ここを通らない適用は、画面と設定が食い違ったままになります。
pub(super) fn take_effect(settings: Settings) {
    settings::apply(settings);
    editor::redraw_all();
}

/// ユーザーが設定を変えたとき: 効かせて、次回起動用のファイルにも書きます。
pub(super) fn change(settings: Settings) {
    take_effect(settings.clone());
    spawn_local(async move {
        write_settings(&settings::write(&settings)).await;
    });
}

#[component]
pub(super) fn Preferences(open: RwSignal<bool>) -> impl IntoView {
    let current = settings::current();
    let font_size = RwSignal::new(current.font_size);
    let font_family = RwSignal::new(current.font_family);
    let caret_blink = RwSignal::new(current.caret_blink);
    let global_shortcut = RwSignal::new(current.global_shortcut);

    let changed = move || {
        change(Settings {
            font_size: font_size.get_untracked(),
            font_family: font_family.get_untracked().trim().to_string(),
            caret_blink: caret_blink.get_untracked(),
            global_shortcut: global_shortcut.get_untracked(),
            ..settings::current()
        });
    };

    view! {
        <div class="prefbar">
            <label class="pref">
                "文字の大きさ"
                <input
                    class="pref-size"
                    type="number"
                    min="8"
                    max="40"
                    prop:value=move || font_size.get().to_string()
                    on:change=move |ev| {
                        if let Ok(size) = event_target_value(&ev).parse::<f64>() {
                            font_size.set(size.clamp(8.0, 40.0));
                            changed();
                        }
                    }
                />
            </label>
            <label class="pref">
                "字体"
                <input
                    class="pref-font"
                    placeholder="既定"
                    prop:value=move || font_family.get()
                    on:change=move |ev| {
                        font_family.set(event_target_value(&ev));
                        changed();
                    }
                />
            </label>
            <label class="pref">
                <input
                    type="checkbox"
                    prop:checked=move || caret_blink.get()
                    on:change=move |ev| {
                        caret_blink.set(event_target_checked(&ev));
                        changed();
                    }
                />
                "カーソルを点滅させる"
            </label>
            <label class="pref">
                <input
                    type="checkbox"
                    prop:checked=move || global_shortcut.get()
                    on:change=move |ev| {
                        global_shortcut.set(event_target_checked(&ev));
                        changed();
                    }
                />
                "Ctrl+Alt+M で呼び出す"
            </label>
            <button class="tool" on:click=move |_| {
                let defaults = Settings {
                    column_gap: settings::current().column_gap,
                    history_limit: settings::current().history_limit,
                    ..Settings::default()
                };
                font_size.set(defaults.font_size);
                font_family.set(defaults.font_family.clone());
                caret_blink.set(defaults.caret_blink);
                global_shortcut.set(defaults.global_shortcut);
                change(defaults);
            }>"既定に戻す"</button>
            <button class="tool" on:click=move |_| open.set(false)>"閉じる"</button>
        </div>
    }
}
