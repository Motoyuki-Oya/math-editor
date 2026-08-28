//! マウス: テキスト内および構造内でクリック、ドラッグ、およびダブルクリックします。

use std::cell::RefCell;
use std::rc::Rc;

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
    if event.button() != 0 {
        return;
    }
    event.prevent_default();
    let adds = input::adds_caret(&event);
    let crossed = super::session::choose_pane(session, adds);
    let (x, y) = (event.client_x() as f64, event.client_y() as f64);
    let hit = hit_at(session, x, y);
    if click_in_structure(session, &hit, event.shift_key(), adds, crossed) {
        session.borrow_mut().dragging = true;
        focus();
        redraw(session);
        return;
    }
    leave_structure(session);
    let pos = {
        let borrowed = session.borrow();
        let doc = borrowed.document.borrow();
        borrowed.view.pos_at_point(doc.text(), x, y)
    };
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
            session.borrow_mut().edit(|editor| editor.select_structure());
        }
        redraw(session);
        return;
    }
    let pos = {
        let borrowed = session.borrow();
        let doc = borrowed.document.borrow();
        borrowed.view.pos_at_point(
            doc.text(),
            event.client_x() as f64,
            event.client_y() as f64,
        )
    };
    session.borrow_mut().edit(|editor| editor.extend_to(pos));
    redraw(session);
}

pub fn on_dblclick(session: &Rc<RefCell<Session>>, event: MouseEvent) {
    // 入れ子Rowでは外側の文書選択へ広げず、そのRowを選択単位にします。
    if session.borrow().nested_cursor().is_some() {
        session.borrow_mut().edit(|editor| editor.select_all());
        redraw(session);
        return;
    }
    let pos = {
        let borrowed = session.borrow();
        let doc = borrowed.document.borrow();
        borrowed.view.pos_at_point(
            doc.text(),
            event.client_x() as f64,
            event.client_y() as f64,
        )
    };
    session.borrow_mut().edit(|editor| {
        editor.set_caret(pos);
        editor.add_next_occurrence();
    });
    redraw(session);
}
