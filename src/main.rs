mod app;
mod editor;
mod format;
mod ipc;
mod settings;
mod structure;
mod view;

use leptos::prelude::*;

fn main() {
    console_error_panic_hook::set_once();
    mount_to_body(|| view! { <app::App/> })
}
