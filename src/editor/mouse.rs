//! マウス: テキスト内および構造内でクリック、ドラッグ、およびダブルクリックします。

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::JsCast;
use web_sys::MouseEvent;

use super::commands::leave_structure;
use super::input;
use super::session::{focus, redraw, Session};
use crate::view::measure::Hit;

/// ビュー内の点の位置、深さに関係なく: テキストと構造について同じ質問が行われ、1 つの答えで両方がカバーされます。
fn hit_at(session: &Rc<RefCell<Session>>, x: f64, y: f64) -> Hit {
    let borrowed = session.borrow();
    let doc = borrowed.document.borrow();
    borrowed.view.hit(doc.text(), x, y)
}

fn is_on_scrollbar(event: &MouseEvent) -> bool {
    let Some(target) = event.target() else {
        return false;
    };
    let Ok(el) = target.dyn_into::<web_sys::HtmlElement>() else {
        return false;
    };
    if el.class_list().contains("mn-vscroll")
        || el.class_list().contains("mn-hscroll")
        || el.class_list().contains("mn-thumb-space")
    {
        return true;
    }
    if el.class_list().contains("mn-aligned-block") {
        let rect = el.get_bounding_client_rect();
        let client_h = el.client_height() as f64;
        let y = event.client_y() as f64;
        if y >= rect.top() + client_h {
            return true;
        }
    }
    if let Some(parent) = el.closest(".mn-aligned-block").ok().flatten() {
        if let Ok(parent_el) = parent.dyn_into::<web_sys::HtmlElement>() {
            let rect = parent_el.get_bounding_client_rect();
            let client_h = parent_el.client_height() as f64;
            let y = event.client_y() as f64;
            if y >= rect.top() + client_h {
                return true;
            }
        }
    }
    false
}

/// クリックした入れ子Rowへキャレットを置き、そこで処理したかを返します。
fn click_in_structure(
    session: &Rc<RefCell<Session>>,
    hit: &Hit,
    extend: bool,
    add: bool,
    replace: bool,
) -> bool {
    let Hit::Inside(at, cursor) = hit else {
        return false;
    };
    session.borrow_mut().edit(|editor| {
        if add {
            return if replace {
                editor.select_nested(*at, cursor.clone())
            } else {
                editor.add_nested(*at, cursor.clone())
            };
        }
        if !extend {
            return editor.enter_at(*at, cursor);
        }
        // 入れ子Row内で始めた選択は、同じ文書行のそのRow内に留めます。
        if editor.nested_cursor().is_none() || editor.primary().head != *at {
            return false;
        }
        editor.extend_nested(cursor)
    })
}

pub fn on_mousedown(session: &Rc<RefCell<Session>>, event: MouseEvent) {
    if event.button() != 0 || is_on_scrollbar(&event) {
        return;
    }
    event.prevent_default();
    let (x, y) = (event.client_x() as f64, event.client_y() as f64);
    let hit = hit_at(session, x, y);
    let pos = {
        let borrowed = session.borrow();
        let doc = borrowed.document.borrow();
        borrowed.view.pos_at_point(doc.text(), x, y)
    };
    let adds = input::adds_caret(&event);
    let crossed = super::session::choose_pane(session, adds);
    if event.detail() == 2 {
        if let Hit::Inside(at, cursor) = &hit {
            session.borrow_mut().edit(|editor| {
                editor.select_nested_word_at(*at, cursor);
            });
        } else if session.borrow().nested_cursor().is_some() {
            session.borrow_mut().edit(|editor| editor.select_all());
        } else {
            leave_structure(session);
            session.borrow_mut().edit(|editor| {
                editor.select_word_at(pos);
            });
        }
        session.borrow_mut().dragging = false;
        focus();
        redraw(session);
        return;
    }

    if event.detail() >= 3 {
        if let Hit::Inside(at, cursor) = &hit {
            session.borrow_mut().edit(|editor| {
                editor.select_nested_row_at(*at, cursor);
            });
        } else {
            leave_structure(session);
            session.borrow_mut().edit(|editor| {
                editor.select_line_at(pos);
            });
        }
        session.borrow_mut().dragging = false;
        focus();
        redraw(session);
        return;
    }

    if click_in_structure(session, &hit, event.shift_key(), adds, crossed) {
        session.borrow_mut().dragging = true;
        focus();
        redraw(session);
        return;
    }
    leave_structure(session);

    session.borrow_mut().dragging = true;
    session.borrow_mut().edit(|editor| {
        if crossed {
            editor.set_caret(pos);
        } else if adds {
            editor.add_caret(pos);
        } else if event.shift_key() {
            editor.extend_to(pos);
        } else {
            editor.set_caret(pos);
        }
    });
    focus();
    redraw(session);
}

pub fn on_mousemove(session: &Rc<RefCell<Session>>, event: MouseEvent) {
    if !session.borrow().dragging {
        return;
    }
    let (x, y) = (event.client_x() as f64, event.client_y() as f64);
    if session.borrow().nested_cursor().is_some() {
        // 入れ子Rowから外へドラッグした場合は、それを含む構造Node全体を選択します。
        let hit = hit_at(session, x, y);
        if !click_in_structure(session, &hit, true, false, false) {
            session
                .borrow_mut()
                .edit(|editor| editor.select_structure());
        }
        redraw(session);
        return;
    }
    let pos = {
        let borrowed = session.borrow();
        let doc = borrowed.document.borrow();
        borrowed
            .view
            .pos_at_point(doc.text(), event.client_x() as f64, event.client_y() as f64)
    };
    session.borrow_mut().edit(|editor| editor.extend_to(pos));
    redraw(session);
}

pub fn on_dblclick(session: &Rc<RefCell<Session>>, event: MouseEvent) {
    if event.button() != 0 || is_on_scrollbar(&event) {
        return;
    }
    event.prevent_default();
    let (x, y) = (event.client_x() as f64, event.client_y() as f64);
    let hit = hit_at(session, x, y);
    let pos = {
        let borrowed = session.borrow();
        let doc = borrowed.document.borrow();
        borrowed.view.pos_at_point(doc.text(), x, y)
    };
    let adds = input::adds_caret(&event);
    super::session::choose_pane(session, adds);
    session.borrow_mut().dragging = false;
    session.borrow_mut().edit(|editor| {
        if event.detail() >= 3 {
            if let Hit::Inside(at, cursor) = &hit {
                editor.select_nested_row_at(*at, cursor);
            } else {
                editor.select_line_at(pos);
            }
        } else if let Hit::Inside(at, cursor) = &hit {
            editor.select_nested_word_at(*at, cursor);
        } else if editor.nested_cursor().is_some() {
            editor.select_all();
        } else {
            editor.select_word_at(pos);
        }
    });
    super::session::focus();
    redraw(session);
}
