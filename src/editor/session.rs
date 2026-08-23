//! ペインごとに 1 つの編集セッション: 誰が画面上にあるか、誰がフォーカスを持っているか、および変更が画面とシェルにどのように到達するかの台帳。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use web_sys::{HtmlElement, HtmlTextAreaElement};

use super::input;
use super::model::{Editor, Flush};
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
    /// 行数の走査がまだ終わっていないか。終わるまで Ctrl+End は保留する。
    pub counting: bool,
    /// 走査完了を待っている Ctrl+End（値は shift）。確定したら跳ぶ。
    pub jump_end: Option<bool>,
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

pub(super) fn pane_session(pane: usize) -> Option<Rc<RefCell<Session>>> {
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
    // 入力欄は横スクロールする要素の中で、行と一緒に動く。
    let textarea = input::build(&doc, &view.scroller())?;
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
        counting: false,
        jump_end: None,
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

/// 画面に入ったのにまだ届いていない行の範囲をアプリへ知らせます。
/// 取り寄せ自体は文書の取っ手を知っているアプリの仕事です。
type OnMissing = Rc<dyn Fn(usize, std::ops::Range<usize>)>;

thread_local! {
    static ON_MISSING: RefCell<Option<OnMissing>> = const { RefCell::new(None) };
}

pub fn set_on_missing(callback: OnMissing) {
    ON_MISSING.with(|slot| *slot.borrow_mut() = Some(callback));
}

/// まだ届いていない行を含む選択のコピーを、文書の本体を知るアプリへ頼みます。
type OnFarCopy = Rc<dyn Fn(usize, super::commands::FarCopy)>;

thread_local! {
    static ON_FAR_COPY: RefCell<Option<OnFarCopy>> = const { RefCell::new(None) };
}

pub fn set_on_far_copy(callback: OnFarCopy) {
    ON_FAR_COPY.with(|slot| *slot.borrow_mut() = Some(callback));
}

pub(super) fn request_far_copy(pane: usize, copy: super::commands::FarCopy) {
    let callback = ON_FAR_COPY.with(|slot| slot.borrow().clone());
    if let Some(callback) = callback {
        callback(pane, copy);
    }
}

/// 描いた窓の中にまだ届いていない行があれば、その範囲を要求します。
fn request_missing(session: &Rc<RefCell<Session>>) {
    let (pane, range) = {
        let borrowed = session.borrow();
        let drawn = borrowed.view.drawn();
        let Some(first) = borrowed.editor.text().first_absent(drawn.start) else {
            return;
        };
        if first >= drawn.end {
            return;
        }
        (borrowed.pane, first..drawn.end)
    };
    let callback = ON_MISSING.with(|slot| slot.borrow().clone());
    if let Some(callback) = callback {
        callback(pane, range);
    }
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
    {
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
    request_missing(session);
}

/// ホイール。窓を行の分だけ動かして描き直します。
pub(super) fn wheel(session: &Rc<RefCell<Session>>, pixels: f64) {
    session.borrow().view.wheel(pixels);
    scrolled(session);
}

/// つまみが動いた。文書全体の割合で窓を動かして描き直します。
pub(super) fn thumb_moved(session: &Rc<RefCell<Session>>) {
    if session.borrow().view.follow_thumb() {
        scrolled(session);
    }
}

/// ビューがスクロールされた後に再度描画するため、表示された行がページに配置されます。 [`redraw`] とは異なり、これはユーザーがスクロールしたビューを残し、キャレットに移動しません。
pub fn scrolled(session: &Rc<RefCell<Session>>) {
    {
        let session = session.borrow();
        let caret = caret_of(&session);
        session.view.repaint(
            session.editor.text(),
            session.editor.sels(),
            &caret,
            session.focused,
        );
    }
    request_missing(session);
}

/// 1 つのキャレットで両方のケースを説明するため、描画に選択するモードはありません。
fn caret_of(session: &Session) -> Caret<'_> {
    Caret {
        at: session.editor.primary().head,
        inside: session.editor.nested_cursor(),
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

impl Parked {
    /// 画面の外にある文書にも、届いた行は同じように入ります。
    pub fn feed(&mut self, from: usize, lines: &[String]) {
        self.editor.feed(
            from,
            lines.iter().map(|line| document::read_line(line)).collect(),
        );
    }

    /// 画面の外にある文書も、本体の巻き戻しに合わせます。
    pub fn apply_restored(&mut self, state: &str, touched_from: usize, line_count: usize) {
        self.editor.apply_restored(state, touched_from, line_count);
    }
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

/// 読み込んだ内容を表示し、文書の本体（1 行の空文書）へまるごと届くようにします。
/// 下書きの復元で使われます。
pub fn load(text: &str) {
    let Some(session) = session() else { return };
    session
        .borrow_mut()
        .editor
        .load_contents(document::read(text));
    changed(&session);
}

/// 行数だけ分かっている文書を出します。行は見えた場所から取り寄せられます。
/// 行数は走査中の途中値なので、確定は [`set_line_count`] で届く。
pub fn load_pending(line_count: usize) {
    let Some(session) = session() else { return };
    {
        let mut borrowed = session.borrow_mut();
        borrowed.editor.load_pending(line_count);
        borrowed.counting = true;
        borrowed.jump_end = None;
    }
    changed(&session);
}

/// 走査で確定した行数をペインの文書へ合わせます。保留していた Ctrl+End が
/// あればここで跳びます。
pub fn set_line_count(pane: usize, count: usize) {
    let Some(session) = pane_session(pane) else { return };
    {
        let mut borrowed = session.borrow_mut();
        borrowed.editor.resize_pending(count);
        borrowed.counting = false;
        if let Some(shift) = borrowed.jump_end.take() {
            borrowed.editor.move_document_edge(true, shift);
        }
    }
    redraw(&session);
}

/// 画面上のペインへ届いた行を入れます。
pub fn feed_pane(pane: usize, from: usize, lines: &[String]) {
    let Some(session) = pane_session(pane) else {
        return;
    };
    session.borrow_mut().editor.feed(
        from,
        lines.iter().map(|line| document::read_line(line)).collect(),
    );
    // 描き直すのは届いた行が画面に見えるときだけ。見えない行のために毎回
    // 描き直すと、取り寄せより描画が高くつく。描き直しはユーザーの置いた
    // スクロールを尊重する。届いた行のためにキャレットへ跳んではいけない。
    let (visible, follows_caret) = {
        let borrowed = session.borrow();
        let drawn = borrowed.view.drawn();
        (
            from < drawn.end && from + lines.len() > drawn.start,
            drawn.contains(&borrowed.editor.primary().head.line),
        )
    };
    if visible {
        session.borrow().view.invalidate();
        if follows_caret {
            // Ctrl+End など、目的の行の中身が届いた。目的行を含む窓を直接
            // 描き直してそこへ着地する。スクロール座標から窓を推定し直さない。
            redraw(&session);
        } else {
            scrolled(&session);
        }
    }
}

/// たまった編集を、文書の本体へ送れる形で渡します。何もなければ `None`。
pub fn take_flush(pane: usize) -> Option<FlushBatch> {
    let session = pane_session(pane)?;
    let mut borrowed = session.borrow_mut();
    take_flush_of(&mut borrowed.editor)
}

/// 入れ替えの行番号は本体側（編集前）の行番号で、後ろの入れ替えから先に
/// 適用すれば前の行番号が狂いません。
fn take_flush_of(editor: &mut Editor) -> Option<FlushBatch> {
    let flush: Flush = editor.take_flush()?;
    let text = editor.text();
    // 入れ替えは今の行番号の昇順で届く。本体の行番号へは、前の入れ替えが
    // 増減させた行数の分だけ戻す。
    let mut delta = 0isize;
    let mut edits: Vec<FlushEdit> = flush
        .changes
        .into_iter()
        .map(|change| {
            let from = (change.from as isize - delta) as usize;
            delta += change.inserted as isize - change.removed as isize;
            FlushEdit {
                from,
                to: from + change.removed,
                lines: (change.from..change.from + change.inserted)
                    .map(|line| match text.raw_line(line) {
                        Some(source) => source.to_string(),
                        None => document::write_line(text.line(line)),
                    })
                    .collect(),
            }
        })
        .collect();
    edits.reverse();
    Some(FlushBatch {
        group: flush.group,
        before: flush.before,
        after: flush.after,
        edits,
    })
}

/// 文書の本体の履歴で 1 ステップになる、編集のひとかたまり。
pub struct FlushBatch {
    pub group: u64,
    pub before: String,
    pub after: String,
    /// 本体側の行番号で表した入れ替え。この順（後ろの行から）で適用する。
    pub edits: Vec<FlushEdit>,
}

pub struct FlushEdit {
    pub from: usize,
    pub to: usize,
    pub lines: Vec<String>,
}

/// 文書の本体が巻き戻ったのに合わせます。
pub fn apply_restored(pane: usize, state: &str, touched_from: usize, line_count: usize) {
    let Some(session) = pane_session(pane) else {
        return;
    };
    session
        .borrow_mut()
        .editor
        .apply_restored(state, touched_from, line_count);
    redraw(&session);
}

pub fn stats() -> (usize, usize) {
    session()
        .map(|session| session.borrow().editor.text().stats())
        .unwrap_or((0, 1))
}

/// 入力を受けるペインの文書が手元に全部あるか。検索や置換が文書の本体の
/// 走査を要るかの見分け。
pub fn fully_resident() -> bool {
    session().is_some_and(|session| session.borrow().editor.text().absent_lines() == 0)
}
