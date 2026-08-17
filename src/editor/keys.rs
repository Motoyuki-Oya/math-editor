//! The keys of the editor itself: what a keystroke in the document means.
//!
//! There is one table: what a key means inside a structure is the model's
//! business, not the keyboard's, so the caret being in one changes nothing
//! here. Keys that drive the application around the editor (files, tabs,
//! panes, the search bar) live in `crate::app`'s own table.

use std::cell::RefCell;
use std::rc::Rc;

use web_sys::KeyboardEvent;

use super::model::Did;
use super::session::{changed, redraw, Session};

pub fn on_keydown(session: &Rc<RefCell<Session>>, event: KeyboardEvent) {
    if session.borrow().composing {
        return;
    }
    let key = event.key();
    let ctrl = event.ctrl_key() || event.meta_key();
    let shift = event.shift_key();
    let did = {
        let mut borrowed = session.borrow_mut();
        let editor = &mut borrowed.editor;
        match (ctrl, key.as_str()) {
            (_, "ArrowLeft") => editor.move_h(false, shift),
            (_, "ArrowRight") => editor.move_h(true, shift),
            (false, "ArrowUp") => editor.move_v(false, shift),
            (false, "ArrowDown") => editor.move_v(true, shift),
            // Alt+Up/Down would move lines; Ctrl adds a caret above or below.
            (true, "ArrowUp") | (true, "ArrowDown") => Did::Nothing,
            (false, "Home") => editor.move_line_edge(false, shift),
            (false, "End") => editor.move_line_edge(true, shift),
            (true, "Home") => editor.move_document_edge(false, shift),
            (true, "End") => editor.move_document_edge(true, shift),
            (false, "Backspace") => editor.backspace(),
            (false, "Delete") => editor.delete_forward(),
            // A grid grows by a row on Alt+Enter or Ctrl+Enter.
            (_, "Enter") if event.alt_key() || ctrl => editor.grow_matrix(),
            (false, "Enter") => editor.split_line(),
            (false, "Escape") => editor.escape(),
            (true, "a") => editor.select_all(),
            (true, "d") => {
                if editor.add_next_occurrence() {
                    Did::Moved
                } else {
                    Did::Nothing
                }
            }
            // Undo, redo and the clipboard are handled once, by the window
            // shortcuts, in the text and in a structure alike.
            (true, _) => Did::Nothing,
            // Tab is the column separator in the text and the next slot inside
            // a structure.
            (false, "Tab") => editor.tab(shift),
            // Printable keys arrive as input events, which also covers the IME.
            (false, _) => Did::Nothing,
        }
    };
    match did {
        Did::Nothing => return,
        Did::Moved => redraw(session),
        Did::Changed => changed(session),
    }
    event.prevent_default();
}
