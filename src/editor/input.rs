//! Keyboard, IME and mouse handling for the editor core.
//!
//! Typing goes through a hidden textarea: the browser gives it the keystrokes
//! and the IME composition, and every change is applied to the model at all of
//! the carets at once.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::convert::FromWasmAbi;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{
    CompositionEvent, Document, HtmlElement, HtmlTextAreaElement, InputEvent, KeyboardEvent,
    MouseEvent,
};

use super::model::Item;
use super::state::{self, Session};

pub fn build(doc: &Document, root: &HtmlElement) -> Option<HtmlTextAreaElement> {
    let textarea = doc
        .create_element("textarea")
        .ok()?
        .dyn_into::<HtmlTextAreaElement>()
        .ok()?;
    textarea.set_class_name("mn-input");
    textarea.set_attribute("autocapitalize", "off").ok();
    textarea.set_attribute("autocomplete", "off").ok();
    textarea.set_attribute("spellcheck", "false").ok();
    textarea.set_attribute("wrap", "off").ok();
    root.append_child(&textarea).ok()?;
    Some(textarea)
}

/// Wires the events that make the editor usable.
pub fn install(session: &Rc<RefCell<Session>>) {
    let textarea = session.borrow().textarea.clone();
    let root = session.borrow().view.root.clone();

    on(
        &textarea,
        "keydown",
        session,
        |session, event: KeyboardEvent| {
            state::on_keydown(session, event);
        },
    );
    on(&textarea, "input", session, |session, event: InputEvent| {
        state::on_input(session, event);
    });
    on(
        &textarea,
        "compositionstart",
        session,
        |session, _: CompositionEvent| {
            session.borrow_mut().composing = true;
            session.borrow_mut().preedit.clear();
            state::sync_input_box(session);
        },
    );
    on(
        &textarea,
        "compositionupdate",
        session,
        |session, event: CompositionEvent| {
            state::update_composition(session, &event.data().unwrap_or_default());
        },
    );
    on(
        &textarea,
        "compositionend",
        session,
        |session, event: CompositionEvent| {
            session.borrow_mut().composing = false;
            let text = event.data().unwrap_or_default();
            state::commit_composition(session, &text);
        },
    );
    on(
        &textarea,
        "blur",
        session,
        |session, _: web_sys::FocusEvent| {
            session.borrow_mut().focused = false;
            state::redraw(session);
        },
    );
    on(
        &textarea,
        "focus",
        session,
        |session, _: web_sys::FocusEvent| {
            session.borrow_mut().focused = true;
            state::redraw(session);
        },
    );

    on(&root, "mousedown", session, |session, event: MouseEvent| {
        state::on_mousedown(session, event);
    });
    on(&root, "mousemove", session, |session, event: MouseEvent| {
        state::on_mousemove(session, event);
    });
    on(&root, "dblclick", session, |session, event: MouseEvent| {
        state::on_dblclick(session, event);
    });
    if let Some(window) = web_sys::window() {
        let target: web_sys::EventTarget = window.into();
        on(&target, "mouseup", session, |session, _: MouseEvent| {
            session.borrow_mut().dragging = false;
        });
    }
    on(
        &textarea,
        "paste",
        session,
        |session, event: web_sys::ClipboardEvent| {
            if let Some(data) = event.clipboard_data().and_then(|d| d.get_data("text").ok()) {
                event.prevent_default();
                state::insert_text(session, &data);
            }
        },
    );
    on(
        &textarea,
        "copy",
        session,
        |session, event: web_sys::ClipboardEvent| {
            copy_selection(session, &event, false);
        },
    );
    on(
        &textarea,
        "cut",
        session,
        |session, event: web_sys::ClipboardEvent| {
            copy_selection(session, &event, true);
        },
    );
}

fn copy_selection(session: &Rc<RefCell<Session>>, event: &web_sys::ClipboardEvent, remove: bool) {
    let text = state::selected_text(session);
    if text.is_empty() {
        return;
    }
    event.prevent_default();
    if let Some(data) = event.clipboard_data() {
        data.set_data("text/plain", &text).ok();
    }
    if remove {
        state::delete_selection(session);
    }
}

/// Adds a listener that borrows the session only while it runs.
fn on<E, T>(
    target: &T,
    name: &str,
    session: &Rc<RefCell<Session>>,
    handler: impl Fn(&Rc<RefCell<Session>>, E) + 'static,
) where
    E: FromWasmAbi + 'static,
    T: AsRef<web_sys::EventTarget>,
{
    let session = session.clone();
    let closure = Closure::<dyn FnMut(E)>::new(move |event: E| handler(&session, event));
    target
        .as_ref()
        .add_event_listener_with_callback(name, closure.as_ref().unchecked_ref())
        .ok();
    closure.forget();
}

/// Whether a mouse event asks for another caret rather than moving the only one.
pub fn adds_caret(event: &MouseEvent) -> bool {
    event.alt_key()
}

/// The text a selection covers, joined with newlines, islands as `$( ... )`.
pub fn text_of(items: Vec<Vec<Item>>) -> String {
    items
        .into_iter()
        .map(|line| {
            line.into_iter()
                .map(|item| match item {
                    Item::Char(c) => c.to_string(),
                    Item::Math { source } => format!("$({source})"),
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}
