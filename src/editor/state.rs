//! Holds the editing session and turns events into model commands.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use web_sys::{HtmlElement, HtmlTextAreaElement, InputEvent, KeyboardEvent, MouseEvent};

use super::clipboard::{self, Clip};
use super::input;
use super::model::{Did, Editor};
use super::search::{self, Place, SearchOptions};
use super::trigger;
use crate::format::document;
use crate::structure::ast::Node;
use crate::view::document::{Caret, View};

pub struct Session {
    /// Names the pane this document is shown in.
    pub pane: usize,
    pub editor: Editor,
    pub view: View,
    pub textarea: HtmlTextAreaElement,
    pub focused: bool,
    pub composing: bool,
    /// What the IME is composing right now, drawn where it will be inserted.
    pub preedit: String,
    pub dragging: bool,
    /// Where the next search carries on from, which may be inside a structure.
    pub search_from: Option<search::Key>,
}

/// Called with the pane whose document changed.
type OnChange = Box<dyn Fn(usize)>;

thread_local! {
    /// One session per pane on screen. Split view is what makes it a list.
    static PANES: RefCell<Vec<Rc<RefCell<Session>>>> = const { RefCell::new(Vec::new()) };
    /// The pane that takes the typing.
    static FOCUSED: Cell<usize> = const { Cell::new(0) };
    static NEXT_PANE: Cell<usize> = const { Cell::new(0) };
    static ON_CHANGE: RefCell<Option<OnChange>> = const { RefCell::new(None) };
}

/// The session of the pane that takes the typing.
pub fn session() -> Option<Rc<RefCell<Session>>> {
    let focused = FOCUSED.get();
    PANES.with(|panes| {
        let panes = panes.borrow();
        panes
            .iter()
            .find(|session| session.borrow().pane == focused)
            .or_else(|| panes.first())
            .cloned()
    })
}

fn pane_session(pane: usize) -> Option<Rc<RefCell<Session>>> {
    PANES.with(|panes| {
        panes
            .borrow()
            .iter()
            .find(|session| session.borrow().pane == pane)
            .cloned()
    })
}

/// Builds an editor inside `root`. The returned number names the pane.
pub fn init(root: &HtmlElement) -> Option<usize> {
    let doc = root.owner_document()?;
    let view = View::new(root.clone())?;
    let textarea = input::build(&doc, root)?;
    let pane = NEXT_PANE.get();
    NEXT_PANE.set(pane + 1);
    let session = Rc::new(RefCell::new(Session {
        pane,
        editor: Editor::default(),
        view,
        textarea,
        focused: false,
        composing: false,
        preedit: String::new(),
        dragging: false,
        search_from: None,
    }));
    input::install(&session);
    PANES.with(|panes| panes.borrow_mut().push(session.clone()));
    if PANES.with(|panes| panes.borrow().len()) == 1 {
        FOCUSED.set(pane);
    }
    redraw(&session);
    Some(pane)
}

/// Drops a pane, when the split is undone.
pub fn close_pane(pane: usize) {
    PANES.with(|panes| {
        panes
            .borrow_mut()
            .retain(|session| session.borrow().pane != pane)
    });
    if FOCUSED.get() == pane {
        if let Some(session) = PANES.with(|panes| panes.borrow().first().cloned()) {
            let pane = session.borrow().pane;
            focus_pane(pane);
        }
    }
}

/// Sends the typing to `pane`.
pub fn focus_pane(pane: usize) {
    if pane_session(pane).is_some() {
        FOCUSED.set(pane);
    }
    focus();
}

/// The pane the events came from is the one that takes the typing.
pub fn note_focus(session: &Rc<RefCell<Session>>) {
    FOCUSED.set(session.borrow().pane);
}

pub fn set_on_change(callback: OnChange) {
    ON_CHANGE.with(|slot| *slot.borrow_mut() = Some(callback));
}

pub fn changed(session: &Rc<RefCell<Session>>) {
    session.borrow_mut().search_from = None;
    redraw(session);
    let pane = session.borrow().pane;
    let callback = ON_CHANGE.with(|slot| slot.borrow_mut().take());
    if let Some(callback) = callback {
        callback(pane);
        ON_CHANGE.with(|slot| *slot.borrow_mut() = Some(callback));
    }
}

pub fn redraw(session: &Rc<RefCell<Session>>) {
    let session = session.borrow();
    // One caret describes both cases, so the drawing has no mode to pick.
    let caret = Caret {
        at: session.editor.primary().head,
        inside: session.editor.inside(),
        composing: (!session.preedit.is_empty()).then_some(session.preedit.as_str()),
    };
    session.view.draw(
        session.editor.text(),
        session.editor.sels(),
        &caret,
        session.focused,
    );
    if let Some(rect) = session.view.reveal(&caret) {
        let style = format!(
            "left:{}px;top:{}px;height:{}px",
            rect.left,
            rect.top,
            rect.height.max(16.0)
        );
        session.textarea.set_attribute("style", &style).ok();
    }
}

/// Keeps the hidden input where the caret is, so IME candidates show up there.
pub fn sync_input_box(session: &Rc<RefCell<Session>>) {
    redraw(session);
}

pub fn focus() {
    let Some(session) = session() else { return };
    let textarea = session.borrow().textarea.clone();
    textarea.focus().ok();
    // Focusing an element that already has the focus fires no event, so the
    // carets would stay hidden if we waited for one.
    if !session.borrow().focused {
        session.borrow_mut().focused = true;
        redraw(&session);
    }
}

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

/// A whole document, kept aside while another tab is shown.
pub struct Parked {
    editor: Editor,
}

/// Takes a pane's document out so that another one can take its place.
pub fn park(pane: usize) -> Option<Parked> {
    let session = pane_session(pane)?;
    let editor = std::mem::take(&mut session.borrow_mut().editor);
    Some(Parked { editor })
}

/// Shows a parked document in `pane`, or an empty one.
pub fn restore(pane: usize, parked: Option<Parked>) {
    let Some(session) = pane_session(pane) else {
        return;
    };
    {
        let mut borrowed = session.borrow_mut();
        borrowed.preedit.clear();
        borrowed.editor = parked.map(|parked| parked.editor).unwrap_or_default();
    }
    changed(&session);
}

pub fn load(text: &str) {
    let Some(session) = session() else { return };
    session.borrow_mut().editor.load(document::read(text));
    changed(&session);
}

pub fn to_document() -> String {
    session()
        .map(|session| document::write(session.borrow().editor.text()))
        .unwrap_or_default()
}

pub fn stats() -> (usize, usize) {
    session()
        .map(|session| session.borrow().editor.text().stats())
        .unwrap_or((0, 1))
}

/// The text a selection puts on the clipboard, which is ordinary text: the
/// piece itself is kept aside, so pasting it back into the editor keeps its
/// shape without the notation ever leaving the file.
pub fn selected_text(session: &Rc<RefCell<Session>>) -> String {
    let borrowed = session.borrow();
    // A selection inside a structure copies that piece of the structure; the
    // clipboard is the same one either way.
    if let Some(row) = borrowed.editor.island_selection() {
        return clipboard::keep(Clip::Row(row));
    }
    let sel = borrowed.editor.primary();
    if sel.is_caret() {
        return String::new();
    }
    let lines = borrowed.editor.text().slice(sel.start(), sel.end());
    clipboard::keep(Clip::Text(lines))
}

pub fn delete_selection(session: &Rc<RefCell<Session>>) {
    session.borrow_mut().editor.backspace();
    changed(session);
}

/// Stops editing a formula, leaving the caret just after it.
pub fn leave_math(session: &Rc<RefCell<Session>>) {
    session.borrow_mut().editor.leave_island();
}

/// Turns keys into commands. There is one table: what a key means inside a
/// structure is the model's business, not the keyboard's, so the caret being in
/// one changes nothing here.
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

/// Puts the caret, or the far end of the selection, where a click landed inside
/// a formula. Returns whether the click was in one.
fn click_in_math(session: &Rc<RefCell<Session>>, x: f64, y: f64, extend: bool) -> bool {
    let Some((at, element)) = session.borrow().view.field_at_point(x, y) else {
        return false;
    };
    let Some(cursor) = crate::view::structure::position_at_point(&element, x, y) else {
        return false;
    };
    let mut borrowed = session.borrow_mut();
    if !extend {
        return borrowed.editor.enter_island_at(at, &cursor);
    }
    // Widening a selection only stays inside the formula it started in.
    if borrowed.editor.inside().is_none() || borrowed.editor.primary().head != at {
        return false;
    }
    borrowed.editor.extend_in_island(&cursor)
}

pub fn on_mousedown(session: &Rc<RefCell<Session>>, event: MouseEvent) {
    if event.button() != 0 {
        return;
    }
    event.prevent_default();
    let (x, y) = (event.client_x() as f64, event.client_y() as f64);
    if !input::adds_caret(&event) && click_in_math(session, x, y, event.shift_key()) {
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
        if !click_in_math(session, x, y, true) {
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
                Place::Text(sel) => borrowed.editor.replace_range(sel.start(), sel.end(), &text),
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
