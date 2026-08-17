//! The commands of the editor: what typing, the IME, the clipboard, the
//! palette and search-and-replace do to the document.

use std::cell::RefCell;
use std::rc::Rc;

use web_sys::InputEvent;

use super::clipboard::{self, Clip};
use super::search::{self, Place, SearchOptions};
use super::session::{changed, focus, redraw, session, Session};
use super::trigger;
use crate::structure::ast::Node;

pub fn on_input(session: &Rc<RefCell<Session>>, event: InputEvent) {
    let textarea = session.borrow().textarea.clone();
    let text = textarea.value();
    if session.borrow().composing {
        // Still being composed; `compositionupdate` draws it until it is done.
        event.stop_propagation();
        return;
    }
    textarea.set_value("");
    if text.is_empty() {
        return;
    }
    insert_text(session, &text);
}

/// Shows what the IME is composing before it is committed.
pub fn update_composition(session: &Rc<RefCell<Session>>, text: &str) {
    session.borrow_mut().preedit = text.to_string();
    redraw(session);
}

pub fn commit_composition(session: &Rc<RefCell<Session>>, text: &str) {
    session.borrow().textarea.set_value("");
    session.borrow_mut().preedit.clear();
    if !text.is_empty() {
        insert_text(session, text);
    }
}

pub fn insert_text(session: &Rc<RefCell<Session>>, text: &str) {
    // Single characters may start a formula; pasted text never does.
    let mut chars = text.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if trigger::type_char(session, c) {
            return;
        }
    }
    // A piece copied out of a document goes back in with the shape it had;
    // text from anywhere else is the characters it is.
    match clipboard::pasted(text) {
        Some(clip) => session.borrow_mut().editor.insert_clip(&clip),
        None => session.borrow_mut().editor.insert_text(text),
    };
    changed(session);
}

/// Puts an island at the caret and starts editing it.
pub fn insert_math() {
    let Some(session) = session() else { return };
    session.borrow_mut().editor.insert_island();
    focus();
    changed(&session);
}

/// Puts a structure from the palette into the formula at the caret, starting a
/// formula when the caret is in ordinary text.
pub fn insert_node(node: Node) {
    let Some(session) = session() else { return };
    {
        // Starting a formula and putting the structure in it is one step, so one
        // undo takes the whole thing back.
        let mut borrowed = session.borrow_mut();
        borrowed.editor.one_step(|editor| {
            if editor.inside().is_none() {
                editor.insert_island();
            }
            editor.insert_in_island(node);
        });
    }
    focus();
    changed(&session);
}

pub fn undo() {
    let Some(session) = session() else { return };
    if session.borrow_mut().editor.undo() {
        changed(&session);
    }
}

pub fn redo() {
    let Some(session) = session() else { return };
    if session.borrow_mut().editor.redo() {
        changed(&session);
    }
}

/// Selects everything where the caret is: the row of the structure it is in,
/// or the whole document. The system's own select-all item would reach the
/// hidden input element instead of the text, so this is an item of our own.
pub fn select_all() {
    let Some(session) = session() else { return };
    session.borrow_mut().editor.select_all();
    focus();
    redraw(&session);
}

/// The text a selection puts on the clipboard, which is ordinary text: the
/// piece itself is kept aside, so pasting it back into the editor keeps its
/// shape without the notation ever leaving the file.
///
/// `None` means nothing is selected. A selection whose text is empty, such as
/// an empty structure, is still a selection and can still be cut.
pub fn selected_text(session: &Rc<RefCell<Session>>) -> Option<String> {
    let borrowed = session.borrow();
    // A selection inside a structure copies that piece of the structure; the
    // clipboard is the same one either way.
    if let Some(row) = borrowed.editor.island_selection() {
        return Some(clipboard::keep(Clip::Row(row)));
    }
    let sel = borrowed.editor.primary();
    if sel.is_caret() {
        return None;
    }
    let lines = borrowed.editor.text().slice(sel.start(), sel.end());
    Some(clipboard::keep(Clip::Text(lines)))
}

pub fn delete_selection(session: &Rc<RefCell<Session>>) {
    session.borrow_mut().editor.backspace();
    changed(session);
}

/// Stops editing a formula, leaving the caret just after it.
pub fn leave_math(session: &Rc<RefCell<Session>>) {
    session.borrow_mut().editor.leave_island();
}

pub fn find_next(query: &str, options: SearchOptions) -> bool {
    let Some(session) = session() else {
        return false;
    };
    let found = {
        let borrowed = session.borrow();
        let from = borrowed.search_from.clone().unwrap_or_else(|| {
            search::key_at(borrowed.editor.primary().end(), borrowed.editor.inside())
        });
        search::find_next(borrowed.editor.text(), query, options, from)
    };
    let Some(found) = found else {
        return false;
    };
    {
        let mut borrowed = session.borrow_mut();
        borrowed.search_from = Some(found.place.end());
        match found.place {
            // A match in a structure is shown inside it, so what is found is
            // what is selected either way.
            Place::Text(sel) => borrowed.editor.set_sels(vec![sel]),
            Place::Inside { at, cursor } => {
                borrowed.editor.select_in_island(at, cursor);
            }
        }
    }
    focus();
    redraw(&session);
    true
}

pub fn replace_all(query: &str, replacement: &str, options: SearchOptions) -> usize {
    let Some(session) = session() else { return 0 };
    leave_math(&session);
    let matches = {
        let borrowed = session.borrow();
        search::find_all(borrowed.editor.text(), query, options)
    };
    if matches.is_empty() {
        return 0;
    }
    {
        let mut borrowed = session.borrow_mut();
        // Replacing back to front keeps the earlier places valid.
        for found in matches.iter().rev() {
            let text = search::expand(&found.groups, replacement, options);
            match &found.place {
                Place::Text(sel) => borrowed.editor.replace_range_with(
                    sel.start(),
                    sel.end(),
                    search::replacement_items(&text),
                ),
                Place::Inside { at, cursor } => {
                    borrowed
                        .editor
                        .replace_in_island(*at, cursor.clone(), &text);
                }
            }
        }
        borrowed.editor.leave_island();
    }
    changed(&session);
    matches.len()
}
