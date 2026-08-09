mod app;
mod doc;
mod editor;
mod ipc;
mod math;

use leptos::prelude::*;

fn main() {
    console_error_panic_hook::set_once();
    mount_to_body(|| view! { <app::App/> })
}
