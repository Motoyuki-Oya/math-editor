//! The 表示 menu: what the text looks like on screen rather than what it says.
//! Carrying a long line on underneath and numbering the lines change nothing
//! about the document, so they belong here and not in the text itself.

use leptos::prelude::*;

use super::preferences::change;
use crate::settings;
use crate::settings::Settings;

#[component]
pub(super) fn DisplayMenu(open: RwSignal<bool>) -> impl IntoView {
    let current = settings::current();
    let wrap = RwSignal::new(current.wrap);
    let line_numbers = RwSignal::new(current.line_numbers);

    view! {
        <div class="menu">
            <button
                class="tool"
                on:mousedown=super::hold_focus
                on:click=move |_| open.update(|open| *open = !*open)
            >"表示"</button>
            <Show when=move || open.get()>
                <div class="menu-items">
                    <label class="menu-item">
                        <input
                            type="checkbox"
                            prop:checked=move || wrap.get()
                            on:change=move |ev| {
                                wrap.set(event_target_checked(&ev));
                                change(Settings { wrap: wrap.get_untracked(), ..settings::current() });
                            }
                        />
                        "折り返す"
                    </label>
                    <label class="menu-item">
                        <input
                            type="checkbox"
                            prop:checked=move || line_numbers.get()
                            on:change=move |ev| {
                                line_numbers.set(event_target_checked(&ev));
                                change(Settings {
                                    line_numbers: line_numbers.get_untracked(),
                                    ..settings::current()
                                });
                            }
                        />
                        "行番号を表示する"
                    </label>
                </div>
            </Show>
        </div>
    }
}
