//! A single editable formula embedded in the document.
//!
//! The field owns a [`MathState`], renders it into its host element and turns
//! key presses into editing commands. Keys the formula cannot use (arrows off
//! the edge, Escape, ...) are handed back to the surrounding text editor.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlElement, KeyboardEvent, MouseEvent};

use super::ast::Node;
use super::edit::{Escape, MathState};
use super::render;

pub const FIELD_CLASS: &str = "mn-field";
const ID_ATTR: &str = "data-field";
const LATEX_ATTR: &str = "data-latex";
const DISPLAY_ATTR: &str = "data-display";

thread_local! {
    static FIELDS: RefCell<HashMap<String, Rc<RefCell<MathState>>>> = RefCell::new(HashMap::new());
    static FOCUSED: RefCell<Option<String>> = const { RefCell::new(None) };
    static NEXT_ID: RefCell<u32> = const { RefCell::new(0) };
    static ON_CHANGE: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
}

/// Called whenever a formula is edited, so the shell can mark the file dirty.
pub fn set_on_change(callback: Box<dyn Fn()>) {
    ON_CHANGE.with(|slot| *slot.borrow_mut() = Some(callback));
}

fn notify_change() {
    ON_CHANGE.with(|slot| {
        if let Some(callback) = slot.borrow().as_ref() {
            callback();
        }
    });
}

fn next_id() -> String {
    NEXT_ID.with(|n| {
        let mut n = n.borrow_mut();
        *n += 1;
        format!("f{n}")
    })
}

fn state_of(id: &str) -> Option<Rc<RefCell<MathState>>> {
    FIELDS.with(|fields| fields.borrow().get(id).cloned())
}

/// Creates the element that represents a formula in the document. The LaTeX
/// lives in an attribute so copy, paste and undo keep working on plain DOM.
pub fn create_element(doc: &web_sys::Document, latex: &str, display: bool) -> Element {
    let element = doc.create_element("span").expect("create field");
    element.set_class_name(FIELD_CLASS);
    element.set_attribute("contenteditable", "false").ok();
    element.set_attribute("tabindex", "0").ok();
    element.set_attribute(LATEX_ATTR, latex).ok();
    if display {
        element.set_attribute(DISPLAY_ATTR, "block").ok();
    }
    element
}

pub fn is_display(host: &Element) -> bool {
    host.get_attribute(DISPLAY_ATTR).as_deref() == Some("block")
}

pub fn latex_of(host: &Element) -> String {
    host.get_attribute(LATEX_ATTR).unwrap_or_default()
}

/// Attaches behaviour to a field element that is already in the document.
/// Re-attaching an element that undo or paste brought back is safe.
pub fn attach(host: &HtmlElement) {
    if host
        .get_attribute(ID_ATTR)
        .is_some_and(|id| state_of(&id).is_some())
    {
        return;
    }
    let id = next_id();
    host.set_attribute(ID_ATTR, &id).ok();
    let state = Rc::new(RefCell::new(MathState::from_latex(&latex_of(host))));
    FIELDS.with(|fields| fields.borrow_mut().insert(id.clone(), state.clone()));
    if is_display(host) {
        host.class_list().add_1("mn-field-block").ok();
    }
    redraw(host, &state.borrow(), false);
    install_listeners(host, &id);
}

fn redraw(host: &HtmlElement, state: &MathState, focused: bool) {
    let cursor = focused.then(|| state.cursor());
    render::render_into(host, state.root(), cursor);
    host.set_attribute(LATEX_ATTR, &state.to_latex()).ok();
    if state.is_empty() {
        host.class_list().add_1("mn-field-empty").ok();
    } else {
        host.class_list().remove_1("mn-field-empty").ok();
    }
}

fn is_focused(id: &str) -> bool {
    FOCUSED.with(|focused| focused.borrow().as_deref() == Some(id))
}

fn install_listeners(host: &HtmlElement, id: &str) {
    let keydown = {
        let host = host.clone();
        let id = id.to_string();
        Closure::<dyn FnMut(KeyboardEvent)>::new(move |event: KeyboardEvent| {
            on_keydown(&host, &id, event);
        })
    };
    host.add_event_listener_with_callback("keydown", keydown.as_ref().unchecked_ref())
        .ok();
    keydown.forget();

    let mousedown = {
        let host = host.clone();
        let id = id.to_string();
        Closure::<dyn FnMut(MouseEvent)>::new(move |event: MouseEvent| {
            let Some(state) = state_of(&id) else { return };
            event.prevent_default();
            if let Some(cursor) =
                render::position_at_point(&host, event.client_x() as f64, event.client_y() as f64)
            {
                state.borrow_mut().set_cursor(cursor);
            }
            host.focus().ok();
            redraw(&host, &state.borrow(), true);
        })
    };
    host.add_event_listener_with_callback("mousedown", mousedown.as_ref().unchecked_ref())
        .ok();
    mousedown.forget();

    let focus = {
        let host = host.clone();
        let id = id.to_string();
        Closure::<dyn FnMut()>::new(move || {
            FOCUSED.with(|focused| *focused.borrow_mut() = Some(id.clone()));
            if let Some(state) = state_of(&id) {
                redraw(&host, &state.borrow(), true);
            }
        })
    };
    host.add_event_listener_with_callback("focus", focus.as_ref().unchecked_ref())
        .ok();
    focus.forget();

    let blur = {
        let host = host.clone();
        let id = id.to_string();
        Closure::<dyn FnMut()>::new(move || {
            if is_focused(&id) {
                FOCUSED.with(|focused| *focused.borrow_mut() = None);
            }
            if let Some(state) = state_of(&id) {
                redraw(&host, &state.borrow(), false);
            }
        })
    };
    host.add_event_listener_with_callback("blur", blur.as_ref().unchecked_ref())
        .ok();
    blur.forget();
}

fn on_keydown(host: &HtmlElement, id: &str, event: KeyboardEvent) {
    let Some(state) = state_of(id) else { return };
    let key = event.key();
    let ctrl = event.ctrl_key() || event.meta_key();
    let mut escape = None;
    {
        let mut state = state.borrow_mut();
        match (ctrl, key.as_str()) {
            (true, "z") if event.shift_key() => {
                state.redo();
            }
            (true, "z") => {
                state.undo();
            }
            (true, "y") => {
                state.redo();
            }
            (true, "Enter") => {
                state.grow_matrix(true);
            }
            (true, _) => return,
            (false, "ArrowLeft") => escape = state.move_left(),
            (false, "ArrowRight") => escape = state.move_right(),
            (false, "ArrowUp") => {
                if !state.move_up() {
                    escape = Some(Escape::Done);
                }
            }
            (false, "ArrowDown") => {
                if !state.move_down() {
                    escape = Some(Escape::Done);
                }
            }
            (false, "Home") => state.move_home(),
            (false, "End") => state.move_end(),
            (false, "Backspace") => escape = state.backspace(),
            (false, "Delete") => state.delete_forward(),
            (false, "Escape") | (false, "Enter") => escape = Some(Escape::Done),
            (false, "Tab") => {
                if event.shift_key() {
                    escape = state.move_left();
                } else {
                    escape = state.move_right();
                }
            }
            (false, "&") => {
                state.grow_matrix(false);
            }
            (false, other) => {
                let mut chars = other.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) if !event.alt_key() => state.insert_char(c),
                    _ => return,
                }
            }
        }
    }
    event.prevent_default();
    event.stop_propagation();
    redraw(host, &state.borrow(), true);
    notify_change();
    if let Some(escape) = escape {
        crate::doc::leave_field(host, escape);
    }
}

/// Inserts a node into the focused formula; used by the toolbar buttons.
pub fn insert_into_focused(node: Node) -> bool {
    let Some(id) = FOCUSED.with(|focused| focused.borrow().clone()) else {
        return false;
    };
    let Some(state) = state_of(&id) else {
        return false;
    };
    let Some(host) = host_of(&id) else {
        return false;
    };
    state.borrow_mut().insert(node);
    redraw(&host, &state.borrow(), true);
    host.focus().ok();
    notify_change();
    true
}

pub fn focused_host() -> Option<HtmlElement> {
    FOCUSED
        .with(|focused| focused.borrow().clone())
        .and_then(|id| host_of(&id))
}

fn host_of(id: &str) -> Option<HtmlElement> {
    web_sys::window()?
        .document()?
        .query_selector(&format!("[{ID_ATTR}=\"{id}\"]"))
        .ok()?
        .and_then(|element| element.dyn_into::<HtmlElement>().ok())
}

/// Moves the caret to the end of a field and focuses it, used when the caret
/// walks into the formula from the text on its right.
pub fn focus_at_end(host: &HtmlElement) {
    focus_at(host, false);
}

pub fn focus_at_start(host: &HtmlElement) {
    focus_at(host, true);
}

fn focus_at(host: &HtmlElement, start: bool) {
    let Some(id) = host.get_attribute(ID_ATTR) else {
        return;
    };
    let Some(state) = state_of(&id) else { return };
    {
        let mut state = state.borrow_mut();
        if start {
            state.move_to_start();
        } else {
            state.move_to_end();
        }
    }
    host.focus().ok();
    redraw(host, &state.borrow(), true);
}
