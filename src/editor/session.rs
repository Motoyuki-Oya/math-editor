//! ペインごとに 1 つの編集セッション: 誰が画面上にあるか、誰がフォーカスを持っているか、および変更が画面とシェルにどのように到達するかの台帳。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use web_sys::{HtmlElement, HtmlTextAreaElement};

use super::input;
use super::model::Editor;
use super::search;
use crate::format::document;
use crate::view::document::{Caret, View};

pub struct Session {
    /// このドキュメントが表示されているペインに名前を付けます。
    pub pane: usize,
    pub editor: Editor,
    pub view: View,
    pub textarea: HtmlTextAreaElement,
    pub focused: bool,
    pub composing: bool,
    /// IME が現在作成している内容、挿入される場所に描画されます。
    pub preedit: String,
    pub dragging: bool,
    /// 次の検索がどこから行われるか。構造の内部にある可能性があります。
    pub search_from: Option<search::Key>,
}

/// ドキュメントが変更されたペインで呼び出されます。呼び出し中に再び変更が起きてもよいよう、台帳の借用の外で呼べる共有の参照で持ちます。
type OnChange = Rc<dyn Fn(usize)>;

thread_local! {
    /// ペインごとに 1 つのセッション画面。分割ビューはリストを作成します。
    static PANES: RefCell<Vec<Rc<RefCell<Session>>>> = const { RefCell::new(Vec::new()) };
    /// 入力を行うペイン。
    static FOCUSED: Cell<usize> = const { Cell::new(0) };
    static NEXT_PANE: Cell<usize> = const { Cell::new(0) };
    static ON_CHANGE: RefCell<Option<OnChange>> = const { RefCell::new(None) };
}

/// 入力を行うペインのセッション。
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

/// `root` 内にエディターを構築します。返された番号はペインに名前を付けます。
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

/// 分割が元に戻されると、ペインを削除します。
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

/// 入力を「ペイン」に送信します。
pub fn focus_pane(pane: usize) {
    if pane_session(pane).is_some() {
        FOCUSED.set(pane);
    }
    focus();
}

/// イベントが発生したペインが、入力を受け取るペインです。
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
    let callback = ON_CHANGE.with(|slot| slot.borrow().clone());
    if let Some(callback) = callback {
        callback(pane);
    }
}

/// 描き直してキャレットの行を見せ、隠しの入力欄をキャレットの場所についていかせます (IME の候補窓がそこに出ます)。
pub fn redraw(session: &Rc<RefCell<Session>>) {
    let session = session.borrow();
    let caret = caret_of(&session);
    session.view.draw(
        session.editor.text(),
        session.editor.sels(),
        &caret,
        session.focused,
    );
    if let Some(rect) = session.view.reveal(&caret) {
        input::follow_caret(&session.textarea, rect);
    }
}

/// ビューがスクロールされた後に再度描画するため、表示された行がページに配置されます。 [`redraw`] とは異なり、これはユーザーがスクロールしたビューを残し、キャレットに移動しません。
pub fn scrolled(session: &Rc<RefCell<Session>>) {
    let session = session.borrow();
    let caret = caret_of(&session);
    session.view.repaint(
        session.editor.text(),
        session.editor.sels(),
        &caret,
        session.focused,
    );
}

/// 1 つのキャレットで両方のケースを説明するため、描画に選択するモードはありません。
fn caret_of(session: &Session) -> Caret<'_> {
    Caret {
        at: session.editor.primary().head,
        inside: session.editor.inside(),
        composing: (!session.preedit.is_empty()).then_some(session.preedit.as_str()),
    }
}

/// すべてのペインを形成する何か (設定) が変更された場合に備えて、すべてのペインを再描画します。
pub fn redraw_all() {
    let sessions = PANES.with(|panes| panes.borrow().clone());
    for session in sessions {
        redraw(&session);
    }
}

pub fn focus() {
    let Some(session) = session() else { return };
    let textarea = session.borrow().textarea.clone();
    textarea.focus().ok();
    // すでにフォーカスがある要素にフォーカスしてもイベントは発生しないため、イベントを待っていてもキャレットは非表示のままになります。
    if !session.borrow().focused {
        session.borrow_mut().focused = true;
        redraw(&session);
    }
}

/// 脇に置かれた文書 1 つ分。別のタブが表示されている間、ここに保ちます。
pub struct Parked {
    editor: Editor,
}

/// ペインのドキュメントを取り出して、別のペインがその場所に移動できるようにします。
pub fn park(pane: usize) -> Option<Parked> {
    let session = pane_session(pane)?;
    let editor = std::mem::take(&mut session.borrow_mut().editor);
    Some(Parked { editor })
}

/// 「ペイン」に保留されているドキュメント、または空のドキュメントを表示します。
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

/// 1 つのペインのドキュメント（入力されているペインのいずれか）。変更元のペインに続くドラフトで使用されます。
pub fn document_of(pane: usize) -> Option<String> {
    let session = pane_session(pane)?;
    let text = document::write(session.borrow().editor.text());
    Some(text)
}

pub fn stats() -> (usize, usize) {
    session()
        .map(|session| session.borrow().editor.text().stats())
        .unwrap_or((0, 1))
}
