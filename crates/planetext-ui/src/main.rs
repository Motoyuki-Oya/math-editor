mod app;
mod editor;
pub(crate) mod font_loader;
mod format;
mod framework;
mod settings;
mod structure;
pub mod syntax;
mod view;

use leptos::prelude::*;

fn main() {
    console_error_panic_hook::set_once();
    mount_to_body(|| view! { <app::App/> })
}
