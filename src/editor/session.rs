//! One editing session per pane: the ledger of who is on screen, who has the
//! focus, and how a change reaches the screen and the shell.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use web_sys::{HtmlElement, HtmlTextAreaElement};

use super::input;
use super::model::Editor;
use super::search;
use crate::format::document;
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
