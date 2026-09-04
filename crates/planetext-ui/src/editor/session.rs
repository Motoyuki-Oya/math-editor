//! ペインごとに 1 つの編集セッション (V): 誰が画面上にあるか、誰がフォーカスを持っているか、および変更が画面とシェルにどのように到達するかの台帳。
//! 各セッションは Document ID を通じて共有の Document Model (M: Rc<RefCell<Editor>>) を参照します。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use web_sys::{HtmlElement, HtmlTextAreaElement};

use super::input;
use super::model::{merge_cursors, Document, Editor, Flush, UnifiedCursor};
use super::search;
use crate::format::document;
use crate::structure::ast::Cursor;
use crate::structure::text::{Pos, Sel};
use crate::view::document::{Caret, Overlay, View};
use crate::view::measure::Hit;

pub struct Session {
    /// このドキュメントが表示されているペインに名前を付けます。
    pub pane: usize,
    /// 現在表示している Document Model の ID。
    pub doc_id: usize,
    /// Document Model (M) への参照。
    pub document: Rc<RefCell<Document>>,
    /// このペイン独自のキャレット・選択範囲 (V)。
    pub cursors: Vec<UnifiedCursor>,
    pub view: View,
    pub textarea: HtmlTextAreaElement,
    pub focused: bool,
    pub composing: bool,
    /// IME が現在作成している内容、挿入される場所に描画されます。
    pub preedit: String,
    pub dragging: bool,
    /// Alt+クリックで別ペインのキャレットと同じ入力グループに入っているか。
    pub linked: bool,
    /// 次の検索がどこから行われるか。構造の内部にある可能性があります。
    pub search_from: Option<search::Key>,
    /// 入力中の検索。可視範囲だけを調べ、移動せずにハイライトする。
    preview_query: String,
    preview_options: search::SearchOptions,
    preview_file_size: Option<usize>,
    preview_found: Vec<search::Found>,
    /// 行数未確定中にEOFから読んだ末尾行。仮位置と再配置用の元文字列。
    pending_tail: Option<(usize, Vec<String>)>,
    /// インライン補完（ゴーストテキスト）の現在候補。
    pub ghost: Option<super::suggest::GhostText>,
    /// 上書き入力モード（Insertキーでトグル）。
    pub overwrite_mode: bool,
}

impl Session {
    pub fn is_counting(&self) -> bool {
        self.document.borrow().is_counting()
    }

    pub fn primary(&self) -> Sel {
        self.cursors.last().expect("at least one cursor").sel
    }

    pub fn primary_cursor(&self) -> &UnifiedCursor {
        self.cursors.last().expect("at least one cursor")
    }

    #[allow(dead_code)]
    pub fn primary_cursor_mut(&mut self) -> &mut UnifiedCursor {
        self.cursors.last_mut().expect("at least one cursor")
    }

    pub fn cursors(&self) -> &[UnifiedCursor] {
        &self.cursors
    }

    pub fn sels(&self) -> Vec<Sel> {
        self.cursors
            .iter()
            .filter(|cursor| cursor.inside.is_none())
            .map(|cursor| cursor.sel)
            .collect()
    }

    pub fn nested_cursor(&self) -> Option<&Cursor> {
        self.primary_cursor().inside.as_ref()
    }

    #[allow(dead_code)]
    pub fn merge_sels(&mut self) {
        merge_cursors(&mut self.cursors);
    }

    /// 一時的に Editor ラッパーを構築して編集・カーソル操作を行います。
    pub fn edit<R>(&mut self, f: impl FnOnce(&mut Editor) -> R) -> R {
        let mut doc = self.document.borrow_mut();
        let mut editor = Editor {
            document: std::mem::take(&mut *doc),
            cursors: std::mem::take(&mut self.cursors),
        };
        let res = f(&mut editor);
        *doc = editor.document;
        self.cursors = editor.cursors;
        res
    }
}

/// ドキュメントが変更されたペインで呼び出されます。呼び出し中に再び変更が起きてもよいよう、台帳の借用の外で呼べる共有の参照で持ちます。
type OnChange = Rc<dyn Fn(usize)>;
pub type OnRedraw = Rc<dyn Fn(usize)>;

thread_local! {
    /// 全ての開いているドキュメントの Model (M)。タブID（doc_id）ごとに 1 つ保持される。
    static DOCUMENTS: RefCell<HashMap<usize, Rc<RefCell<Document>>>> = RefCell::new(HashMap::new());
    /// ドキュメントのファイルパス（拡張子からの構文判定に使用）。
    static DOC_PATHS: RefCell<HashMap<usize, String>> = RefCell::new(HashMap::new());
    /// ペインごとに 1 つのセッション画面 (V)。分割ビューはリストを作成します。
    static PANES: RefCell<Vec<Rc<RefCell<Session>>>> = const { RefCell::new(Vec::new()) };
    /// 入力を行うペイン。
    static FOCUSED: Cell<usize> = const { Cell::new(0) };
    static NEXT_PANE: Cell<usize> = const { Cell::new(0) };
    static ON_CHANGE: RefCell<Option<OnChange>> = const { RefCell::new(None) };
    static ON_REDRAW: RefCell<Vec<OnRedraw>> = const { RefCell::new(Vec::new()) };
}

pub fn add_on_redraw(callback: OnRedraw) {
    ON_REDRAW.with(|slot| slot.borrow_mut().push(callback));
}

fn notify_redraw(pane: usize) {
    let callbacks = ON_REDRAW.with(|slot| slot.borrow().clone());
    for callback in callbacks {
        callback(pane);
    }
}

/// 指定IDの Document Model を取得、無ければ新規作成して返します。
pub fn get_or_create_doc(id: usize) -> Rc<RefCell<Document>> {
    DOCUMENTS.with(|docs| {
        docs.borrow_mut()
            .entry(id)
            .or_insert_with(|| Rc::new(RefCell::new(Document::default())))
            .clone()
    })
}

/// ドキュメントのファイルパスを登録または更新します。
pub fn set_doc_path(doc_id: usize, path: Option<String>) {
    DOC_PATHS.with(|paths| {
        if let Some(p) = path {
            paths.borrow_mut().insert(doc_id, p);
        } else {
            paths.borrow_mut().remove(&doc_id);
        }
    });
    redraw_doc(doc_id, Some(FOCUSED.get()));
}

/// ドキュメントのファイルパスを取得します。
pub fn doc_path(doc_id: usize) -> Option<String> {
    DOC_PATHS.with(|paths| paths.borrow().get(&doc_id).cloned())
}

/// タブがすべてのペインから破棄された際に、Document Model を解放します。
pub fn release_doc(id: usize) {
    DOCUMENTS.with(|docs| {
        docs.borrow_mut().remove(&id);
    });
    DOC_PATHS.with(|paths| {
        paths.borrow_mut().remove(&id);
    });
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

/// ペインに特定のドキュメント (tab.id) をバインドして表示します。
pub fn bind_doc(pane: usize, doc_id: usize) {
    let Some(session) = pane_session(pane) else {
        return;
    };
    let doc = get_or_create_doc(doc_id);
    {
        let mut borrowed = session.borrow_mut();
        borrowed.doc_id = doc_id;
        borrowed.document = doc;
        borrowed.cursors = vec![UnifiedCursor::caret(Pos::default())];
        borrowed.preedit.clear();
        borrowed.search_from = None;
        borrowed.pending_tail = None;
    }
    redraw(&session);
}

/// root 内にエディターを構築します。返された番号はペインに名前を付けます。
pub fn init(root: &HtmlElement) -> Option<usize> {
    let doc = root.owner_document()?;
    let view = View::new(root.clone())?;
    // 入力欄は横スクロールする要素の中で、行と一緒に動く。
    let textarea = input::build(&doc, &view.scroller())?;
    let pane = NEXT_PANE.get();
    NEXT_PANE.set(pane + 1);
    let doc_id = 0;
    let document = get_or_create_doc(doc_id);
    let session = Rc::new(RefCell::new(Session {
        pane,
        doc_id,
        document,
        cursors: vec![UnifiedCursor::caret(Pos::default())],
        view,
        textarea,
        focused: false,
        composing: false,
        preedit: String::new(),
        dragging: false,
        linked: false,
        search_from: None,
        preview_query: String::new(),
        preview_options: search::SearchOptions::default(),
        preview_file_size: None,
        preview_found: Vec::new(),
        pending_tail: None,
        ghost: None,
        overwrite_mode: false,
    }));
    input::install(&session);
    PANES.with(|panes| panes.borrow_mut().push(session.clone()));
    if PANES.with(|panes| panes.borrow().len()) == 1 {
        FOCUSED.set(pane);
    }
    redraw(&session);
    Some(pane)
}

pub fn toggle_overwrite_mode(session: &Rc<RefCell<Session>>) -> bool {
    let new_mode = {
        let mut borrowed = session.borrow_mut();
        borrowed.overwrite_mode = !borrowed.overwrite_mode;
        borrowed.overwrite_mode
    };
    redraw(session);
    new_mode
}

pub fn is_focused_overwrite_mode() -> bool {
    session()
        .map(|s| s.borrow().overwrite_mode)
        .unwrap_or(false)
}

pub fn reset_all_overwrite_modes() {
    PANES.with(|panes| {
        for session in panes.borrow().iter() {
            session.borrow_mut().overwrite_mode = false;
            redraw(session);
        }
    });
}

/// 分割が元に戻されると、ペインを削除します。
pub fn close_pane(pane: usize) {
    PANES.with(|panes| {
        panes
            .borrow_mut()
            .retain(|session| session.borrow().pane != pane)
    });
    let linked = PANES.with(|panes| {
        panes
            .borrow()
            .iter()
            .filter(|session| session.borrow().linked)
            .count()
    });
    if linked < 2 {
        clear_linked();
    }
    if FOCUSED.get() == pane {
        if let Some(session) = PANES.with(|panes| panes.borrow().first().cloned()) {
            let pane = session.borrow().pane;
            focus_pane(pane);
        }
    }
}

/// フォーカスが変わった際にアプリケーション側へ通知します。
type OnFocus = Rc<dyn Fn(usize)>;

thread_local! {
    static ON_FOCUS: RefCell<Option<OnFocus>> = const { RefCell::new(None) };
}

pub fn set_on_focus(callback: OnFocus) {
    ON_FOCUS.with(|slot| *slot.borrow_mut() = Some(callback));
}

fn notify_focus(pane: usize) {
    let callback = ON_FOCUS.with(|slot| slot.borrow().clone());
    if let Some(callback) = callback {
        callback(pane);
    }
}

/// 入力を「ペイン」に送信します。
pub fn focus_pane(pane: usize) {
    clear_linked();
    if pane_session(pane).is_some() {
        FOCUSED.set(pane);
        notify_focus(pane);
    }
    focus();
}

/// イベントが発生したペインが、入力を受け取るペインです。
pub fn note_focus(session: &Rc<RefCell<Session>>) {
    let pane = session.borrow().pane;
    FOCUSED.set(pane);
    notify_focus(pane);
}

/// クリックしたペインを入力先にする。Alt+クリックで別ペインへ移った場合は、
/// 元のペインとクリック先を同じ入力グループへ入れる。通常クリックは解除する。
pub fn choose_pane(session: &Rc<RefCell<Session>>, add: bool) -> bool {
    let pane = session.borrow().pane;
    let was_linked = session.borrow().linked;
    let previous = FOCUSED.replace(pane);
    notify_focus(pane);
    let crossed = add && previous != pane;
    let newly_linked = crossed && !was_linked;
    let sessions = PANES.with(|panes| panes.borrow().clone());
    if crossed {
        for target in &sessions {
            let target_pane = target.borrow().pane;
            if target_pane == previous || target_pane == pane {
                target.borrow_mut().linked = true;
            }
        }
    } else if !add {
        for target in &sessions {
            target.borrow_mut().linked = false;
        }
    }
    for target in sessions {
        if target.borrow().pane != pane {
            scrolled(&target);
        }
    }
    newly_linked
}

/// 1回の編集を受けるペイン群。連動中でなければ発生元だけ。
pub(super) fn edit_sessions(origin: &Rc<RefCell<Session>>) -> Vec<Rc<RefCell<Session>>> {
    if !origin.borrow().linked {
        return vec![origin.clone()];
    }
    PANES.with(|panes| {
        panes
            .borrow()
            .iter()
            .filter(|session| session.borrow().linked)
            .cloned()
            .collect()
    })
}

pub(super) fn clear_linked() {
    let sessions = PANES.with(|panes| panes.borrow().clone());
    for session in sessions {
        if session.borrow().linked {
            session.borrow_mut().linked = false;
            redraw(&session);
        }
    }
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

/// 行数未確定中のCtrl+Endが、EOF基準の末尾読みをアプリへ頼む入口。
type OnTail = Rc<dyn Fn(usize)>;

thread_local! {
    static ON_TAIL: RefCell<Option<OnTail>> = const { RefCell::new(None) };
}

pub fn set_on_tail(callback: OnTail) {
    ON_TAIL.with(|slot| *slot.borrow_mut() = Some(callback));
}

pub(super) fn request_tail(pane: usize) {
    let callback = ON_TAIL.with(|slot| slot.borrow().clone());
    if let Some(callback) = callback {
        callback(pane);
    }
}

pub(super) fn tail_locked(session: &Rc<RefCell<Session>>) -> bool {
    let borrowed = session.borrow();
    borrowed.is_counting()
        && borrowed.pending_tail.as_ref().is_some_and(|(from, lines)| {
            let line = borrowed.primary().head.line;
            line >= *from && line < *from + lines.len()
        })
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
        let doc = borrowed.document.borrow();
        let drawn = borrowed.view.drawn();
        let Some(first) = doc.text().first_absent(drawn.start) else {
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

/// 指定ペインの検索欄の入力を可視範囲へ反映する。選択やキャレットは変更しない。
pub fn preview_search_pane(pane: usize, query: &str, options: search::SearchOptions) -> usize {
    let Some(session) = pane_session(pane) else {
        return 0;
    };
    {
        let mut borrowed = session.borrow_mut();
        borrowed.preview_query = query.to_string();
        borrowed.preview_options = options;
        refresh_preview(&mut borrowed);
    }
    redraw_preview_overlay(&session);
    let count = session.borrow().preview_found.len();
    count
}

pub fn clear_search_preview_pane(pane: usize) {
    let Some(session) = pane_session(pane) else {
        return;
    };
    {
        let mut borrowed = session.borrow_mut();
        borrowed.preview_query.clear();
        borrowed.preview_found.clear();
    }
    redraw_preview_overlay(&session);
}

#[allow(dead_code)]
pub fn preview_search(query: &str, options: search::SearchOptions) -> usize {
    if let Some(focused_session) = session() {
        let pane = focused_session.borrow().pane;
        preview_search_pane(pane, query, options)
    } else {
        0
    }
}

pub fn clear_search_preview() {
    if let Some(focused_session) = session() {
        let pane = focused_session.borrow().pane;
        clear_search_preview_pane(pane);
    }
}

fn redraw_preview_overlay(session: &Rc<RefCell<Session>>) {
    let session = session.borrow();
    let doc = session.document.borrow();
    let caret = caret_of(&session);
    let carets = carets_of(&session);
    let highlights = preview_highlights(&session);
    let modified = doc.modified_lines();
    let sels = session.sels();
    let line_count = doc.text().line_count();
    let path = doc_path(session.doc_id);
    let lang = path.as_deref().and_then(crate::syntax::for_path);
    session.view.redraw_overlay(
        line_count,
        &Overlay {
            sels: &sels,
            highlights: &highlights,
            modified: &modified,
            primary: &caret,
            carets: &carets,
            focused: session.focused,
            linked: session.linked,
            overwrite: session.overwrite_mode,
            show_numbers: !session.is_counting(),
            language: lang.as_ref(),
            ghost: session.ghost.as_ref(),
        },
    );
}

fn refresh_preview(session: &mut Session) {
    if session.preview_query.is_empty() {
        session.preview_found.clear();
        return;
    }
    let doc = session.document.borrow();
    session.preview_found = search::find_range(
        doc.text(),
        &session.preview_query,
        session.preview_options,
        session.preview_file_size,
        session.view.drawn(),
    );
}

fn preview_highlights(session: &Session) -> Vec<crate::view::document::Highlight> {
    use search::Place;
    session
        .preview_found
        .iter()
        .map(|found| match &found.place {
            Place::Text(sel) => crate::view::document::Highlight {
                line: sel.start().line,
                path: Vec::new(),
                from: sel.start().col,
                to: sel.end().col,
            },
            Place::Inside { at, cursor } => crate::view::document::Highlight {
                line: at.line,
                path: cursor.path.clone(),
                from: cursor.start(),
                to: cursor.end(),
            },
        })
        .collect()
}

pub fn changed(session: &Rc<RefCell<Session>>) {
    let doc_id = session.borrow().doc_id;
    let pane = session.borrow().pane;
    session.borrow_mut().search_from = None;

    // 同じ doc_id を表示しているすべてのセッション（View）を再描画
    redraw_doc(doc_id, Some(pane));

    let callback = ON_CHANGE.with(|slot| slot.borrow().clone());
    if let Some(callback) = callback {
        callback(pane);
    }
}

/// 同一の Document Model を表示しているすべてのペイン（View）を再描画します。
pub fn redraw_doc(doc_id: usize, origin_pane: Option<usize>) {
    let sessions = PANES.with(|panes| {
        panes
            .borrow()
            .iter()
            .filter(|s| s.borrow().doc_id == doc_id)
            .cloned()
            .collect::<Vec<_>>()
    });
    for s in sessions {
        let is_origin = origin_pane.is_some_and(|p| p == s.borrow().pane);
        if is_origin {
            redraw(&s);
        } else {
            s.borrow().view.invalidate();
            scrolled(&s);
        }
    }
}

/// キャレット位置のゴーストテキスト候補を再計算します。
pub fn update_ghost_text(session: &mut Session) {
    if !session.focused || !session.preedit.is_empty() || session.cursors.len() != 1 {
        session.ghost = None;
        return;
    }
    let primary = session.primary();
    if !primary.is_caret() || session.primary_cursor().inside.is_some() {
        session.ghost = None;
        return;
    }
    let pos = primary.head;
    let doc = session.document.borrow();
    let line_row = doc.text().line(pos.line).to_vec();
    let plain_line = crate::structure::plain::row(&line_row);

    let prefix = super::suggest::extract_prefix(&plain_line, pos.col);
    let has_kanji = super::suggest::is_kanji_preceding(&line_row, pos.col);

    if prefix.is_none() && !has_kanji {
        session.ghost = None;
        return;
    }

    let path = doc_path(session.doc_id);
    let mut lang = path.as_deref().and_then(crate::syntax::for_path);
    if lang.as_ref().is_some_and(|l| l.name == "Markdown") {
        if let Some(Some(name)) = super::suggest::markdown_code_block_lang(doc.text(), pos.line) {
            if let Some(resolved) = crate::syntax::for_name(&name)
                .or_else(|| crate::syntax::for_path(&format!("virtual.{name}")))
            {
                lang = Some(resolved);
            }
        }
    }

    let start_scan = pos.line.saturating_sub(40);
    let end_scan = (pos.line + 40).min(doc.text().line_count());
    let buffer_words = if prefix.is_some() {
        Some(super::suggest::collect_buffer_words_range(
            doc.text(),
            start_scan..end_scan,
        ))
    } else {
        None
    };
    let buffer_rubies = if has_kanji {
        Some(super::suggest::collect_buffer_rubies_range(
            doc.text(),
            start_scan..end_scan,
        ))
    } else {
        None
    };

    session.ghost = super::suggest::find_suggestion(
        pos.line,
        pos.col,
        &line_row,
        &plain_line,
        lang.as_ref(),
        buffer_words.as_ref(),
        buffer_rubies.as_ref(),
    );
}

/// ゴーストテキスト候補を確定挿入します。
pub fn accept_suggestion(session: &Rc<RefCell<Session>>) -> bool {
    let ghost = {
        let mut borrowed = session.borrow_mut();
        borrowed.ghost.take()
    };
    let Some(ghost) = ghost else {
        return false;
    };
    {
        let mut borrowed = session.borrow_mut();
        borrowed.edit(|editor| {
            if let Some((kanji_len, reading)) = &ghost.ruby {
                editor.apply_ruby(*kanji_len, reading);
            } else {
                editor.insert_text(&ghost.suffix);
            }
        });
    }
    changed(session);
    true
}

/// 現在ゴーストテキスト候補が存在するか判定します。
pub fn has_ghost_text(session: &Session) -> bool {
    session.ghost.is_some()
}

/// ゴーストテキスト候補を消去します。
pub fn clear_ghost_text(session: &mut Session) -> bool {
    let had = session.ghost.is_some();
    session.ghost = None;
    had
}

/// 描き直してキャレットの行を見せ、隠しの入力欄をキャレットの場所についていかせます (IME の候補窓がそこに出ます)。
pub fn redraw(session: &Rc<RefCell<Session>>) {
    {
        update_ghost_text(&mut session.borrow_mut());
        let session = session.borrow();
        let doc = session.document.borrow();
        let caret = caret_of(&session);
        let carets = carets_of(&session);
        let highlights = preview_highlights(&session);
        let modified = doc.modified_lines();
        let sels = session.sels();
        let path = doc_path(session.doc_id);
        let lang = path.as_deref().and_then(crate::syntax::for_path);
        session.view.draw(
            doc.text(),
            &Overlay {
                sels: &sels,
                highlights: &highlights,
                modified: &modified,
                primary: &caret,
                carets: &carets,
                focused: session.focused,
                linked: session.linked,
                overwrite: session.overwrite_mode,
                show_numbers: !session.is_counting(),
                language: lang.as_ref(),
                ghost: session.ghost.as_ref(),
            },
        );
        if let Some(rect) = session.view.reveal(&caret) {
            input::follow_caret(&session.textarea, rect);
        }
    }
    // Ctrl+End などで窓が移った場合も、移動後の drawn 範囲を検索する。
    if !session.borrow().preview_query.is_empty() {
        refresh_preview(&mut session.borrow_mut());
        redraw_preview_overlay(session);
    }
    request_missing(session);
    let pane = session.borrow().pane;
    notify_redraw(pane);
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

/// ビューがスクロールされた後に再度描画するため、表示された行がページに配置されます。 redraw とは異なり、これはユーザーがスクロールしたビューを残し、キャレットに移動しません。
pub fn scrolled(session: &Rc<RefCell<Session>>) {
    {
        let session = session.borrow();
        let doc = session.document.borrow();
        let caret = caret_of(&session);
        let carets = carets_of(&session);
        let highlights = preview_highlights(&session);
        let modified = doc.modified_lines();
        let sels = session.sels();
        let path = doc_path(session.doc_id);
        let lang = path.as_deref().and_then(crate::syntax::for_path);
        session.view.repaint(
            doc.text(),
            &Overlay {
                sels: &sels,
                highlights: &highlights,
                modified: &modified,
                primary: &caret,
                carets: &carets,
                focused: session.focused,
                linked: session.linked,
                overwrite: session.overwrite_mode,
                show_numbers: !session.is_counting(),
                language: lang.as_ref(),
                ghost: session.ghost.as_ref(),
            },
        );
    }
    // repaint で新しい窓が確定してから、その窓を検索して重ねだけを更新する。
    // 先に検索すると、1つ前の drawn 範囲をハイライトしてしまう。
    refresh_preview(&mut session.borrow_mut());
    redraw_preview_overlay(session);
    request_missing(session);
    let pane = session.borrow().pane;
    notify_redraw(pane);
}

pub(super) fn move_visual(
    session: &Rc<RefCell<Session>>,
    right: bool,
    extend: bool,
) -> Option<super::model::Did> {
    let target = {
        let borrowed = session.borrow();
        if borrowed.cursors.len() != 1 {
            return None;
        }
        let cursor = borrowed.primary_cursor();
        if !extend
            && (!cursor.sel.is_caret()
                || cursor
                    .inside
                    .as_ref()
                    .is_some_and(|inside| !inside.is_caret()))
        {
            return None;
        }
        borrowed.view.visual_neighbor(&caret_of(&borrowed), right)?
    };
    let did = session.borrow_mut().edit(|editor| match target {
        Hit::Text(at) => {
            let current = editor.primary();
            if !extend && current.is_caret() && editor.nested_cursor().is_none() {
                let entered = if at.line == current.head.line && at.col == current.head.col + 1 {
                    editor.enter_node(current.head, true)
                } else if at.line == current.head.line && at.col + 1 == current.head.col {
                    editor.enter_node(at, false)
                } else {
                    false
                };
                if entered {
                    return super::model::Did::Moved;
                }
            }
            if extend {
                let anchor = current.anchor;
                editor.set_sels(vec![Sel { anchor, head: at }]);
            } else {
                editor.set_caret(at);
            }
            super::model::Did::Moved
        }
        Hit::Inside(at, cursor) => {
            let moved = if extend {
                editor.extend_nested(&cursor)
            } else {
                editor.enter_at(at, &cursor)
            };
            super::model::Did::moved(moved)
        }
    });
    Some(did)
}

/// 1 つのキャレットで両方のケースを説明するため、描画に選択するモードはありません。
fn caret_of<'a>(session: &'a Session) -> Caret<'a> {
    Caret {
        at: session.primary().head,
        inside: session.nested_cursor(),
        composing: (!session.preedit.is_empty()).then_some(session.preedit.as_str()),
    }
}

fn carets_of<'a>(session: &'a Session) -> Vec<Caret<'a>> {
    let last = session.cursors.len().saturating_sub(1);
    session
        .cursors
        .iter()
        .enumerate()
        .map(|(index, cursor)| Caret {
            at: cursor.sel.head,
            inside: cursor.inside.as_ref(),
            composing: (index == last && !session.preedit.is_empty())
                .then_some(session.preedit.as_str()),
        })
        .collect()
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

/// 読み込んだ内容を表示し、文書の本体（1 行の空文書）へまるごと届くようにします。
/// 下書きの復元で使われます。
#[allow(dead_code)]
pub fn load(text: &str) {
    let Some(session) = session() else { return };
    session
        .borrow_mut()
        .edit(|editor| editor.load_contents(document::read(text)));
    changed(&session);
}

/// 保留状態を開始します。line_count が 0 のときは行数未確定（走査中）として扱われます。
pub fn load_pending(line_count: usize) {
    let Some(session) = session() else { return };
    session
        .borrow()
        .document
        .borrow_mut()
        .load_pending(line_count);
    session.borrow_mut().pending_tail = None;
    changed(&session);
}

/// 指定ドキュメントの保留状態を開始します。line_count が 0 のときは行数未確定（走査中）として扱われます。
pub fn load_pending_doc(doc_id: usize, line_count: usize) {
    let doc = get_or_create_doc(doc_id);
    doc.borrow_mut().load_pending(line_count);
    PANES.with(|panes| {
        for session in panes.borrow().iter() {
            let mut borrowed = session.borrow_mut();
            if borrowed.doc_id == doc_id {
                borrowed.pending_tail = None;
            }
        }
    });
}

/// 行数未確定でも、EOFから届いた末尾ウィンドウを仮の末尾へ表示する。
pub fn show_tail(pane: usize, lines: &[String]) {
    let Some(session) = pane_session(pane) else {
        return;
    };
    let from = {
        let mut borrowed = session.borrow_mut();
        borrowed.edit(|editor| {
            if editor.text().line_count() < lines.len() {
                editor.resize_pending(lines.len());
            }
            let count = editor.text().line_count();
            let from = count.saturating_sub(lines.len());
            editor.forget_range(from..count);
            editor.feed(
                from,
                lines.iter().map(|line| document::read_line(line)).collect(),
            );
            let last = count.saturating_sub(1);
            let col = editor.text().line_len(last);
            editor.set_caret(Pos::new(last, col));
            from
        })
    };
    {
        let mut borrowed = session.borrow_mut();
        borrowed.pending_tail = borrowed.is_counting().then(|| (from, lines.to_vec()));
    }
    redraw(&session);
}

fn apply_line_count(editor: &mut Editor, pending_tail: Option<(usize, Vec<String>)>, count: usize) {
    if let Some((from, lines)) = &pending_tail {
        editor.forget_range(*from..*from + lines.len());
    }
    editor.resize_pending(count);
    if let Some((_, lines)) = pending_tail {
        let from = count.saturating_sub(lines.len());
        editor.forget_range(from..count);
        editor.feed(
            from,
            lines.iter().map(|line| document::read_line(line)).collect(),
        );
        let last = count.saturating_sub(1);
        let col = editor.text().line_len(last);
        editor.set_caret(Pos::new(last, col));
    }
}

/// 走査で確定した行数をペインの文書へ合わせます。仮表示した末尾は、
/// 確定した絶対行番号へ付け替えます。
pub fn set_line_count(pane: usize, count: usize) {
    let Some(session) = pane_session(pane) else {
        return;
    };
    let pending_tail = session.borrow_mut().pending_tail.take();
    session.borrow_mut().edit(|editor| {
        apply_line_count(editor, pending_tail, count);
    });
    redraw(&session);
}

/// 手元に置いておく行数の上限と、見えている窓の周りに残す幅。上限を超えたら
/// 窓から遠い行を未着へ戻し、スクロールで訪れた行が溜まり続けないようにする。
const RESIDENT_LIMIT: usize = 20_000;
const RESIDENT_KEEP: usize = 5_000;

/// 画面上のペインへ届いた行を入れます。
pub fn feed_pane(pane: usize, from: usize, lines: &[String]) {
    let Some(session) = pane_session(pane) else {
        return;
    };
    session.borrow().document.borrow_mut().feed(
        from,
        lines.iter().map(|line| document::read_line(line)).collect(),
    );
    {
        let borrowed = session.borrow();
        let mut doc = borrowed.document.borrow_mut();
        if doc.resident_lines() > RESIDENT_LIMIT {
            let mut min_start = borrowed.view.drawn().start;
            let mut max_end = borrowed.view.drawn().end;
            let mut pinned: Vec<usize> = Vec::new();
            let sessions = PANES.with(|panes| panes.borrow().clone());
            for s in &sessions {
                if s.borrow().doc_id == borrowed.doc_id {
                    let d = s.borrow().view.drawn();
                    min_start = min_start.min(d.start);
                    max_end = max_end.max(d.end);
                    for sel in s.borrow().cursors() {
                        pinned.push(sel.start().line);
                        pinned.push(sel.end().line);
                    }
                }
            }
            doc.evict_far(
                min_start.saturating_sub(RESIDENT_KEEP)..max_end + RESIDENT_KEEP,
                &pinned,
            );
        }
    }
    // 描き直すのは届いた行が画面に見えるときだけ。
    let (visible, follows_caret) = {
        let borrowed = session.borrow();
        let drawn = borrowed.view.drawn();
        (
            from < drawn.end && from + lines.len() > drawn.start,
            drawn.contains(&borrowed.primary().head.line),
        )
    };
    if visible {
        session.borrow().view.invalidate();
        if follows_caret {
            redraw(&session);
        } else {
            scrolled(&session);
        }
    }
}

/// たまった編集を、文書の本体へ送れる形で渡します。何もなければ None。
pub fn take_flush(pane: usize) -> Option<FlushBatch> {
    let session = pane_session(pane)?;
    let mut borrowed = session.borrow_mut();
    borrowed.edit(take_flush_of)
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

/// ステータスバー等で表示するドキュメントおよびキャレット・選択範囲の統計情報。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocStats {
    /// 10MB以下の通常ファイルにおける全体文字数（10MB超の巨大ファイル時は None）。
    pub total_chars: Option<usize>,
    /// 巨大ファイル等のファイルサイズ（バイト数）。
    pub file_bytes: Option<usize>,
    /// 全体行数。
    pub total_lines: usize,
    /// バックグラウンドで行数走査中かどうか。
    pub counting: bool,
    /// 行数未確定の末尾表示中かどうか。
    pub pending_tail: bool,
    /// キャレットの1-based行番号。
    pub caret_line: usize,
    /// キャレットの1-based列番号。
    pub caret_col: usize,
    /// 先頭からキャレットまでの文字数 (改行抜文字数, 改行文字数)。10MB超または未着行時は None。
    pub caret_prefix: Option<(usize, usize)>,
    /// 選択範囲の統計情報（選択が存在する場合のみ Some）。
    pub selection: Option<SelectionStats>,
}

impl Default for DocStats {
    fn default() -> Self {
        DocStats {
            total_chars: Some(0),
            file_bytes: None,
            total_lines: 1,
            counting: false,
            pending_tail: false,
            caret_line: 1,
            caret_col: 1,
            caret_prefix: Some((0, 0)),
            selection: None,
        }
    }
}

/// 選択範囲の統計情報。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionStats {
    /// 選択行数（1行選択なら 1）。
    pub lines: usize,
    /// 選択中の (改行抜文字数, 改行文字数)。10MB超または未着行時は None。
    pub chars: Option<(usize, usize)>,
}

const MAX_CARET_STATS_BYTES: usize = 10_000_000;

pub fn stats() -> DocStats {
    let Some(session) = session() else {
        return DocStats::default();
    };

    let borrowed = session.borrow();
    let doc = borrowed.document.borrow();
    let text = doc.text();
    let line_count = text.line_count();
    let is_counting = borrowed.is_counting();
    let is_large =
        doc.file_bytes.is_some_and(|s| s > MAX_CARET_STATS_BYTES) || text.absent_lines() > 0;

    let primary = borrowed.primary();
    let head = primary.head;
    let caret_line = head.line + 1;
    let caret_col = head.col + 1;

    let total_chars = if is_large || is_counting {
        None
    } else {
        let (chars, _) = text.stats();
        if chars <= MAX_CARET_STATS_BYTES {
            Some(chars)
        } else {
            None
        }
    };

    let caret_prefix = if !is_large && !is_counting && total_chars.is_some() {
        text.chars_until(head)
    } else {
        None
    };

    let sels = borrowed.sels();
    let has_selection = sels.iter().any(|s| !s.is_caret());

    let selection = if has_selection {
        let mut total_chars_without_nl = 0;
        let mut total_newlines = 0;
        let mut total_lines = 0;
        let mut any_absent = false;

        for sel in &sels {
            if sel.is_caret() {
                continue;
            }
            let start = sel.start();
            let end = sel.end();
            let lines = end.line.saturating_sub(start.line) + 1;
            total_lines += lines;

            if !any_absent {
                if lines <= 1000 {
                    if let Some((c, nl)) = text.chars_between(start, end) {
                        total_chars_without_nl += c;
                        total_newlines += nl;
                    } else {
                        any_absent = true;
                    }
                } else {
                    any_absent = true;
                }
            }
        }

        let chars = if any_absent {
            None
        } else {
            Some((total_chars_without_nl, total_newlines))
        };

        Some(SelectionStats {
            lines: total_lines,
            chars,
        })
    } else {
        None
    };

    let pending_tail = borrowed.pending_tail.is_some();

    DocStats {
        total_chars,
        file_bytes: doc.file_bytes,
        total_lines: if is_counting { 0 } else { line_count },
        counting: is_counting,
        pending_tail,
        caret_line,
        caret_col,
        caret_prefix,
        selection,
    }
}

/// ドキュメントのファイルサイズを設定します（巨大ファイルの Zero-Scan 判定用）。
pub fn set_doc_file_size(doc_id: usize, bytes: Option<usize>) {
    let doc = get_or_create_doc(doc_id);
    doc.borrow_mut().set_file_bytes(bytes);
}

/// 入力を受けるペインの文書が手元に全部あるか。検索や置換が文書の本体の
/// 走査を要るかの見分け。
pub fn fully_resident() -> bool {
    session().is_some_and(|session| session.borrow().document.borrow().text().absent_lines() == 0)
}

/// 保存などで文書がファイルと一致した際、変更行マーカーをクリアします。
pub fn clear_modified(pane: usize) {
    if let Some(session) = pane_session(pane) {
        let doc_id = session.borrow().doc_id;
        clear_modified_doc(doc_id);
    }
}

pub fn clear_modified_doc(doc_id: usize) {
    let doc = get_or_create_doc(doc_id);
    doc.borrow_mut().clear_modified();
    redraw_doc(doc_id, Some(FOCUSED.get()));
}

/// 文書の本体が巻き戻ったのに合わせる: 該当ドキュメントを開いているセッションへ
/// テキストを合わせ、カーソルを復元する。
pub fn apply_restored(doc_id: usize, state: &str, touched_from: usize, line_count: usize) {
    let sessions = PANES.with(|panes| panes.borrow().clone());
    let mut text_restored = false;

    for s in &sessions {
        if s.borrow().doc_id == doc_id {
            s.borrow_mut().edit(|editor| {
                if !text_restored {
                    editor.apply_restored(state, touched_from, line_count);
                    text_restored = true;
                } else {
                    editor.restore_state(state);
                }
            });
        }
    }

    if !text_restored {
        let doc = get_or_create_doc(doc_id);
        let mut editor = Editor {
            document: std::mem::take(&mut *doc.borrow_mut()),
            cursors: vec![],
        };
        editor.apply_restored(state, touched_from, line_count);
        *doc.borrow_mut() = editor.document;
    }

    redraw_doc(doc_id, Some(FOCUSED.get()));
}

#[derive(Clone, Debug, PartialEq)]
pub struct UrlTooltip {
    pub url: String,
    pub left: f64,
    pub top: f64,
}

pub fn url_at_caret(pane: usize) -> Option<UrlTooltip> {
    let session = pane_session(pane)?;
    let borrowed = session.borrow();
    let doc = borrowed.document.borrow();
    let caret = caret_of(&borrowed);
    let (url, rect) = borrowed.view.url_at_caret(doc.text(), &caret)?;
    Some(UrlTooltip {
        url,
        left: rect.left,
        top: rect.top,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 【回帰防止テスト】
    /// 巨大ファイルを行数確定前に開いて Ctrl+End を押した際、EOF基準で取得した末尾行が
    /// 一旦仮の位置に表示され、その後のバックグラウンド走査完了（set_line_count）で
    /// 真の絶対行番号へ正しく付け替えられ、キャレットも追従することを保証する。
    #[test]
    fn apply_line_count_correctly_remaps_tail_lines_and_moves_caret() {
        let mut editor = Editor::default();
        // 1. 初期スキャン行数は露出させず、未確定（0行）で保留状態を開始する
        editor.load_pending(0);

        // 2. 末尾2行を要求して表示（show_tail 相当）
        let tail_lines = vec!["tail 0".to_string(), "tail 1".to_string()];
        if editor.text().line_count() < tail_lines.len() {
            editor.resize_pending(tail_lines.len());
        }
        let count = editor.text().line_count();
        let from = count.saturating_sub(tail_lines.len());
        editor.forget_range(from..count);
        editor.feed(
            from,
            tail_lines
                .iter()
                .map(|line| document::read_line(line))
                .collect(),
        );
        let last = count.saturating_sub(1);
        editor.set_caret(Pos::new(last, 6));

        assert_eq!(editor.text().raw_line(last), Some("tail 1"));

        // 3. バックグラウンド走査完了で 16,000,000 行へ付け替え
        let pending_tail = Some((from, tail_lines));
        let final_count = 16_000_000;
        apply_line_count(&mut editor, pending_tail, final_count);

        // 確定後の検証:
        // - 全体行数が 16,000,000 行になっていること
        assert_eq!(editor.text().line_count(), 16_000_000);
        // - 仮配置されていた位置（1）は未着（Line::Absent）に戻っていること
        assert!(editor.text().is_absent(1));
        // - 真の末尾（15999999, 15999998）に正しく末尾行が移動・配置されていること
        assert_eq!(editor.text().raw_line(15_999_999), Some("tail 1"));
        assert_eq!(editor.text().raw_line(15_999_998), Some("tail 0"));
        // - キャレットも真の末尾行（15999999）へ移動していること
        assert_eq!(editor.primary().head.line, 15_999_999);
    }

    /// 【回帰防止テスト】
    /// 下書き復元等で行数が確定（!counting）していても、末尾行が手元に未着（is_absent）なら、
    /// 末尾Seek判定が true になり EOF からの即時末尾取得へ向かうことを保証する。
    #[test]
    fn absent_tail_triggers_eof_tail_seek_even_when_not_counting() {
        let mut editor = Editor::default();
        // 下書き復元で確定行数 16,000,000 行で開かれた状態（counting = false）
        editor.load_pending(16_000_000);
        editor.document.counting = false;

        let count = editor.text().line_count();
        let last_line = count.saturating_sub(1);
        // 末尾行はまだ手元に届いていない（未着）
        assert!(editor.text().is_absent(last_line));

        // 末尾Seekが必要かどうかの判定（keys.rs の Ctrl+End 判定ロジックと同一）
        let needs_tail = editor.document.is_counting() || editor.text().is_absent(last_line);
        assert!(needs_tail, "未着の末尾に対しては必ず末尾Seekが発火すること");
    }
}
