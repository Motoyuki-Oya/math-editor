//! The settings bar. It only shows what a writer would actually change: the
//! size and face of the text, and whether the caret blinks. Everything else
//! in `crate::settings` stays in the file, out of the way.

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::editor;
use crate::ipc;
use crate::settings;
use crate::settings::Settings;

/// Puts `settings` into effect everywhere: on screen, in the editors that
/// measure their own text, and in the file for the next start.
pub(super) fn change(settings: Settings) {
    settings::apply(settings.clone());
    editor::redraw_all();
    spawn_local(async move {
        ipc::write_settings(&settings::write(&settings)).await;
    });
}

#[component]
pub(super) fn Preferences(open: RwSignal<bool>) -> impl IntoView {
    let current = settings::current();
    let font_size = RwSignal::new(current.font_size);
    let font_family = RwSignal::new(current.font_family);
    let caret_blink = RwSignal::new(current.caret_blink);

    let changed = move || {
        change(Settings {
            font_size: font_size.get_untracked(),
            font_family: font_family.get_untracked().trim().to_string(),
            caret_blink: caret_blink.get_untracked(),
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
            <button class="tool" on:click=move |_| {
                let defaults = Settings {
                    column_gap: settings::current().column_gap,
                    history_limit: settings::current().history_limit,
                    ..Settings::default()
                };
                font_size.set(defaults.font_size);
                font_family.set(defaults.font_family.clone());
                caret_blink.set(defaults.caret_blink);
                change(defaults);
            }>"既定に戻す"</button>
            <button class="tool" on:click=move |_| open.set(false)>"閉じる"</button>
        </div>
    }
}
