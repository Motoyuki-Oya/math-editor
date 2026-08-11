//! Holds the editing session and turns events into model commands.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use web_sys::{HtmlElement, HtmlTextAreaElement, InputEvent, KeyboardEvent, MouseEvent};

use super::input;
use super::model::{Editor, Inside, Item, Pos};
use super::search::{self, SearchOptions};
use super::trigger;
use crate::format::document;
use crate::structure::ast::Node;
use crate::structure::edit::{Editing, Escape};
use crate::view::document::{ActiveMath, View};

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
    pub search_from: Option<Pos>,
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
    let caret = session.editor.primary().head;
    let active = session
        .editor
        .inside()
        .map(|cursor| ActiveMath { at: caret, cursor });
    let preedit = (!session.preedit.is_empty()).then_some((caret, session.preedit.as_str()));
    session.view.draw(
        session.editor.text(),
        session.editor.sels(),
        active,
        session.focused,
        preedit,
    );
    if let Some(rect) = session.view.reveal(caret) {
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
    if session.borrow().editor.inside().is_some() {
        // Typing arrives one character at a time, so shortcuts still work; a
        // paste arrives whole and goes in as it is.
        let mut chars = text.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => {
                session.borrow_mut().editor.type_in_island(c);
            }
            _ => {
                let nodes = document::read_row(text);
                session.borrow_mut().editor.insert_row_in_island(nodes);
            }
        }
        changed(session);
        return;
    }
    // Single characters may start a formula; pasted text never does.
    let mut chars = text.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if trigger::type_char(session, c) {
            return;
        }
    }
    session.borrow_mut().editor.insert_text(text);
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
    if session.borrow().editor.inside().is_none() {
        insert_math();
    }
    let Some(session) = self::session() else {
        return;
    };
    session.borrow_mut().editor.insert_in_island(node);
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

pub fn selected_text(session: &Rc<RefCell<Session>>) -> String {
    let borrowed = session.borrow();
    // A selection inside a structure copies that piece of the structure; the
    // clipboard is the same one either way.
    if let Some(row) = borrowed.editor.island_selection() {
        return document::write_row(&row);
    }
    let sel = borrowed.editor.primary();
    if sel.is_caret() {
        return String::new();
    }
    input::text_of(borrowed.editor.text().slice(sel.start(), sel.end()))
}

pub fn delete_selection(session: &Rc<RefCell<Session>>) {
    if in_island(session, Inside::Change, |editing| {
        editing.backspace();
        None
    }) {
        return;
    }
    session.borrow_mut().editor.backspace();
    changed(session);
}

/// Stops editing a formula, leaving the caret just after it.
pub fn leave_math(session: &Rc<RefCell<Session>>) {
    session.borrow_mut().editor.leave_island();
}

/// Starts editing the formula at `at`, from either end.
pub fn enter_math(session: &Rc<RefCell<Session>>, at: Pos, from_start: bool) -> bool {
    session.borrow_mut().editor.enter_island(at, from_start)
}

pub fn on_keydown(session: &Rc<RefCell<Session>>, event: KeyboardEvent) {
    if session.borrow().composing {
        return;
    }
    if session.borrow().editor.inside().is_some() {
        keydown_in_math(session, event);
        return;
    }
    let key = event.key();
    let ctrl = event.ctrl_key() || event.meta_key();
    let shift = event.shift_key();
    let handled = match (ctrl, key.as_str()) {
        (_, "ArrowLeft") => move_h(session, false, shift),
        (_, "ArrowRight") => move_h(session, true, shift),
        (false, "ArrowUp") => act(session, |editor| editor.move_v(false, shift)),
        (false, "ArrowDown") => act(session, |editor| editor.move_v(true, shift)),
        // Alt+Up/Down would move lines; Ctrl adds a caret above or below.
        (true, "ArrowUp") | (true, "ArrowDown") => false,
        (false, "Home") => act(session, |editor| editor.move_line_edge(false, shift)),
        (false, "End") => act(session, |editor| editor.move_line_edge(true, shift)),
        (true, "Home") => act(session, |editor| editor.move_document_edge(false, shift)),
        (true, "End") => act(session, |editor| editor.move_document_edge(true, shift)),
        (false, "Backspace") => edit(session, Editor::backspace),
        (false, "Delete") => edit(session, Editor::delete_forward),
        (false, "Enter") => edit(session, Editor::split_line),
        (false, "Escape") => act(session, |editor| {
            editor.collapse_sels();
        }),
        (true, "a") => act(session, Editor::select_all),
        (true, "d") => act(session, |editor| {
            editor.add_next_occurrence();
        }),
        // Undo and redo are handled once, by the window shortcuts.
        (true, _) => false,
        // Tab is the column separator, which lines up with the neighbouring lines.
        (false, "Tab") => edit(session, Editor::insert_tab),
        (false, other) => {
            // Printable keys arrive as input events, which also covers the IME.
            let _ = other;
            false
        }
    };
    if handled {
        event.prevent_default();
    }
}

/// Runs a model command and redraws, without marking the file dirty.
fn act(session: &Rc<RefCell<Session>>, command: impl FnOnce(&mut Editor)) -> bool {
    command(&mut session.borrow_mut().editor);
    redraw(session);
    true
}

/// Runs a model command that changes the text, so the file becomes dirty.
fn edit(session: &Rc<RefCell<Session>>, command: impl FnOnce(&mut Editor)) -> bool {
    command(&mut session.borrow_mut().editor);
    changed(session);
    true
}

/// Moving across a formula steps into it instead of over it.
fn move_h(session: &Rc<RefCell<Session>>, forward: bool, extend: bool) -> bool {
    if !extend {
        let sel = session.borrow().editor.primary();
        if sel.is_caret() && session.borrow().editor.sels().len() == 1 {
            let at = if forward {
                Some(sel.head)
            } else {
                super::model::before_col(sel.head)
            };
            if let Some(at) = at {
                let is_math = matches!(
                    session.borrow().editor.text().item_at(at),
                    Some(Item::Math(_))
                );
                if is_math && enter_math(session, at, forward) {
                    redraw(session);
                    return true;
                }
            }
        }
    }
    act(session, |editor| editor.move_h(forward, extend))
}

/// The same keys as in the text, applied to the structure the caret is in.
/// Walking out of it is the command's own answer, so there is no second place
/// that decides where the caret ends up.
fn keydown_in_math(session: &Rc<RefCell<Session>>, event: KeyboardEvent) {
    let key = event.key();
    let ctrl = event.ctrl_key() || event.meta_key();
    let shift = event.shift_key();
    // A grid grows by a row on Alt+Enter, wherever the caret is inside it.
    if (event.alt_key() || ctrl) && key == "Enter" {
        in_island(session, Inside::Change, |editing| {
            editing.grow_matrix(true);
            None
        });
        event.prevent_default();
        return;
    }
    if ctrl && key == "a" {
        in_island(session, Inside::Extend, |editing| {
            editing.select_row();
            None
        });
        event.prevent_default();
        return;
    }
    // Undo, redo and the clipboard belong to the document, the same as in text.
    if ctrl {
        return;
    }
    let handled = match key.as_str() {
        // Shift selects, the same as it does in the text. A selection that
        // outgrows the formula becomes a selection of the formula itself.
        "ArrowLeft" if shift => in_island(session, Inside::Extend, |editing| editing.extend(false)),
        "ArrowRight" if shift => in_island(session, Inside::Extend, |editing| editing.extend(true)),
        "ArrowLeft" => in_island(session, Inside::Move, |editing| editing.move_left()),
        "ArrowRight" => in_island(session, Inside::Move, |editing| editing.move_right()),
        "ArrowUp" => in_island(session, Inside::Move, |editing| {
            (!editing.move_up()).then_some(Escape::Done)
        }),
        "ArrowDown" => in_island(session, Inside::Move, |editing| {
            (!editing.move_down()).then_some(Escape::Done)
        }),
        "Home" => in_island(session, Inside::Move, |editing| {
            editing.move_home();
            None
        }),
        "End" => in_island(session, Inside::Move, |editing| {
            editing.move_end();
            None
        }),
        "Backspace" => in_island(session, Inside::Change, |editing| editing.backspace()),
        "Delete" => in_island(session, Inside::Change, |editing| {
            editing.delete_forward();
            None
        }),
        "Escape" | "Enter" => in_island(session, Inside::Move, |_| Some(Escape::Done)),
        "Tab" => in_island(session, Inside::Move, |editing| {
            if shift {
                editing.move_left()
            } else {
                editing.move_right()
            }
        }),
        "&" => in_island(session, Inside::Change, |editing| {
            editing.grow_matrix(false);
            None
        }),
        " " => in_island(session, Inside::Change, |editing| {
            if !editing.commit_command() {
                editing.insert_char(' ');
            }
            None
        }),
        other => {
            let mut chars = other.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if !event.alt_key() => {
                    in_island(session, Inside::Type, move |editing| {
                        editing.insert_char(c);
                        None
                    })
                }
                _ => false,
            }
        }
    };
    if handled {
        event.prevent_default();
    }
}

/// Runs a command on the island the caret is in and redraws. Only a command
/// that changed the structure makes the file dirty.
fn in_island(
    session: &Rc<RefCell<Session>>,
    kind: Inside,
    command: impl FnOnce(&mut Editing<'_>) -> Option<Escape>,
) -> bool {
    if !session.borrow_mut().editor.in_island(kind, command) {
        return false;
    }
    match kind {
        Inside::Move | Inside::Extend => redraw(session),
        Inside::Type | Inside::Change => changed(session),
    }
    true
}

pub fn on_mousedown(session: &Rc<RefCell<Session>>, event: MouseEvent) {
    if event.button() != 0 {
        return;
    }
    event.prevent_default();
    let (x, y) = (event.client_x() as f64, event.client_y() as f64);
    let field = session.borrow().view.field_at_point(x, y);
    if let Some((at, element)) = field {
        if !input::adds_caret(&event) && enter_math(session, at, true) {
            if let Some(cursor) = crate::view::structure::position_at_point(&element, x, y) {
                session
                    .borrow_mut()
                    .editor
                    .in_island(Inside::Move, |editing| {
                        editing.set_cursor(cursor);
                        None
                    });
            }
            focus();
            redraw(session);
            return;
        }
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
    if session.borrow().editor.inside().is_some() {
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
    leave_math(&session);
    let found = {
        let borrowed = session.borrow();
        let from = borrowed
            .search_from
            .unwrap_or_else(|| borrowed.editor.primary().end());
        search::find_next(borrowed.editor.text(), query, options, from)
    };
    let Some(sel) = found else {
        return false;
    };
    {
        let mut borrowed = session.borrow_mut();
        borrowed.editor.set_sels(vec![sel]);
        borrowed.search_from = Some(sel.end());
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
        for (sel, with) in matches.iter().rev() {
            let text = search::expand(with, replacement, options);
            borrowed.editor.replace_range(sel.start(), sel.end(), &text);
        }
    }
    changed(&session);
    matches.len()
}
