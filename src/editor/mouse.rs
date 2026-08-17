//! The mouse: clicks, drags and double clicks, in the text and in a structure.

use std::cell::RefCell;
use std::rc::Rc;

use web_sys::MouseEvent;

use super::commands::leave_math;
use super::input;
use super::session::{focus, redraw, Session};
use crate::view::measure::Hit;

/// Where a point in the view is, at whatever depth: the same question is asked
/// of the text and of a structure, and one answer covers both.
fn hit_at(session: &Rc<RefCell<Session>>, x: f64, y: f64) -> Hit {
    let borrowed = session.borrow();
    borrowed.view.hit(borrowed.editor.text(), x, y)
}

/// Puts the caret, or the far end of the selection, where a click landed inside
/// a formula. Returns whether the click was in one.
fn click_in_math(session: &Rc<RefCell<Session>>, hit: &Hit, extend: bool) -> bool {
    let Hit::Inside(at, cursor) = hit else {
        return false;
    };
    let mut borrowed = session.borrow_mut();
    if !extend {
        return borrowed.editor.enter_island_at(*at, cursor);
    }
    // Widening a selection only stays inside the formula it started in.
    if borrowed.editor.inside().is_none() || borrowed.editor.primary().head != *at {
        return false;
    }
    borrowed.editor.extend_in_island(cursor)
}

pub fn on_mousedown(session: &Rc<RefCell<Session>>, event: MouseEvent) {
    if event.button() != 0 {
        return;
    }
    event.prevent_default();
    let (x, y) = (event.client_x() as f64, event.client_y() as f64);
    let hit = hit_at(session, x, y);
    if !input::adds_caret(&event) && click_in_math(session, &hit, event.shift_key()) {
        session.borrow_mut().dragging = true;
        focus();
        redraw(session);
        return;
    }
    leave_math(session);
    let pos = {
        let borrowed = session.borrow();
        borrowed.view.pos_at_point(borrowed.editor.text(), x, y)
    };
    {
        let mut borrowed = session.borrow_mut();
        borrowed.dragging = true;
        if input::adds_caret(&event) {
            borrowed.editor.add_caret(pos);
        } else if event.shift_key() {
            borrowed.editor.extend_to(pos);
        } else {
            borrowed.editor.set_caret(pos);
        }
    }
    focus();
    redraw(session);
}

pub fn on_mousemove(session: &Rc<RefCell<Session>>, event: MouseEvent) {
    if !session.borrow().dragging {
        return;
    }
    let (x, y) = (event.client_x() as f64, event.client_y() as f64);
    if session.borrow().editor.inside().is_some() {
        // Dragging inside a formula selects inside it; dragging out of it takes
        // the formula as a whole, which is one item of the text.
        let hit = hit_at(session, x, y);
        if !click_in_math(session, &hit, true) {
            session.borrow_mut().editor.select_island();
        }
        redraw(session);
        return;
    }
    let pos = {
        let borrowed = session.borrow();
        borrowed.view.pos_at_point(
            borrowed.editor.text(),
            event.client_x() as f64,
            event.client_y() as f64,
        )
    };
    session.borrow_mut().editor.extend_to(pos);
    redraw(session);
}

pub fn on_dblclick(session: &Rc<RefCell<Session>>, event: MouseEvent) {
    // Inside a formula there are no words to take, so the row is the unit.
    if session.borrow().editor.inside().is_some() {
        session.borrow_mut().editor.select_all();
        redraw(session);
        return;
    }
    let pos = {
        let borrowed = session.borrow();
        borrowed.view.pos_at_point(
            borrowed.editor.text(),
            event.client_x() as f64,
            event.client_y() as f64,
        )
    };
    {
        let mut borrowed = session.borrow_mut();
        borrowed.editor.set_caret(pos);
        borrowed.editor.add_next_occurrence();
    }
    redraw(session);
}
