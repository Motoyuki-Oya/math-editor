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
type OnFocus = Rc<dyn Fn(usize)>;
type OnMissing = Rc<dyn Fn(usize, std::ops::Range<usize>)>;
type OnTail = Rc<dyn Fn(usize)>;
type OnFarCopy = Rc<dyn Fn(usize, super::commands::FarCopy)>;

thread_local! {
    /// 全ての開いているドキュメントの Model (M)。タブID（doc_id）ごとに 1 つ保持される。
    /// テキスト本体、Undo/Redo 履歴、変更行、既知の revision を一元管理する。
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
    static ON_FOCUS: RefCell<Option<OnFocus>> = const { RefCell::new(None) };
    static ON_MISSING: RefCell<Option<OnMissing>> = const { RefCell::new(None) };
    static ON_TAIL: RefCell<Option<OnTail>> = const { RefCell::new(None) };
    static ON_FAR_COPY: RefCell<Option<OnFarCopy>> = const { RefCell::new(None) };
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

/// 指定IDの Document を取得、無ければ新規作成して返します。
pub fn get_or_create_doc(doc_id: usize) -> Rc<RefCell<Document>> {
    DOCUMENTS.with(|docs| {
        docs.borrow_mut()
            .entry(doc_id)
            .or_insert_with(|| Rc::new(RefCell::new(Document::default())))
            .clone()
    })
}

/// 既存コード互換用エイリアス。
pub fn get_or_create_doc_model(doc_id: usize) -> Rc<RefCell<Document>> {
    get_or_create_doc(doc_id)
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

const UNBOUND_DOC_ID: usize = usize::MAX;

/// ペインに特定のドキュメント (tab.id) をバインドして表示します。
/// 複数ペインが同一の doc_id を開く場合、同じ Document への参照 (Rc<RefCell<Document>>) を共有します。
pub fn bind_doc(pane: usize, doc_id: usize) {
    let Some(session) = pane_session(pane) else {
        return;
    };

    let prev_doc_id = session.borrow().doc_id;
    if prev_doc_id == doc_id && prev_doc_id != UNBOUND_DOC_ID {
        redraw(&session);
        return;
    }

    let doc = get_or_create_doc(doc_id);

    {
        let mut borrowed = session.borrow_mut();
        borrowed.doc_id = doc_id;
        borrowed.document = Rc::clone(&doc);
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
    let doc_id = UNBOUND_DOC_ID;
    let document = Rc::new(RefCell::new(Document::default()));
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

pub fn pane_count() -> usize {
    PANES.with(|panes| panes.borrow().len())
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

/// 指定ペインと連動中（Alt+クリック）の全ペインIDを返します。
/// 連動していなければ指定ペインのみを返します。
pub fn linked_panes(origin_pane: usize) -> Vec<usize> {
    PANES.with(|panes| {
        let panes = panes.borrow();
        let origin = panes.iter().find(|s| s.borrow().pane == origin_pane);
        match origin {
            Some(s) if s.borrow().linked => panes
                .iter()
                .filter(|s| s.borrow().linked)
                .map(|s| s.borrow().pane)
                .collect(),
            _ => vec![origin_pane],
        }
    })
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

/// 連動編集トランザクション:
/// 連動する全セッションに関与する Document Model の入力グループ (grouping) を揃えて直列化し、
/// 全ての編集が完了した後にまとめて変更通知を行います。
/// これにより、同一ドキュメントを開く複数スライスでの連動編集が 1 回の Undo ステップ (同一 group ID) に結合されます。
pub(super) fn run_linked_transaction<R>(
    origin: &Rc<RefCell<Session>>,
    f: impl FnOnce(&[Rc<RefCell<Session>>]) -> R,
) -> R {
    let sessions = edit_sessions(origin);
    if sessions.len() < 2 {
        return f(&sessions);
    }

    // 重複を除いた Document リストを抽出
    let mut docs = Vec::new();
    for s in &sessions {
        let doc = s.borrow().document.clone();
        if !docs
            .iter()
            .any(|d: &Rc<RefCell<Document>>| Rc::ptr_eq(d, &doc))
        {
            docs.push(doc);
        }
    }

    let mut groupings = Vec::with_capacity(docs.len());
    for doc in &docs {
        groupings.push(doc.borrow_mut().begin_group());
    }

    let result = f(&sessions);

    for (doc, was_grouping) in docs.iter().zip(groupings) {
        doc.borrow_mut().end_group(was_grouping);
    }

    result
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

pub fn set_on_missing(callback: OnMissing) {
    ON_MISSING.with(|slot| *slot.borrow_mut() = Some(callback));
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
    let pane = session.borrow().pane;
    let doc_id = session.borrow().doc_id;
    session.borrow_mut().search_from = None;

    redraw(session);

    if doc_id != UNBOUND_DOC_ID {
        PANES.with(|panes| {
            for s in panes.borrow().iter() {
                if s.borrow().pane != pane && s.borrow().doc_id == doc_id {
                    s.borrow().view.invalidate();
                    scrolled(s);
                }
            }
        });
    }

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

/// 疎（スライス）テキストとして読み込みます。line_count が None のときは走査中として扱われます。
pub fn load_sparse(line_count: Option<usize>) {
    let Some(session) = session() else { return };
    let doc_id = session.borrow().doc_id;
    if doc_id != UNBOUND_DOC_ID {
        load_sparse_doc(doc_id, line_count);
    } else {
        session
            .borrow()
            .document
            .borrow_mut()
            .load_sparse(line_count);
        session.borrow_mut().pending_tail = None;
        changed(&session);
    }
}

/// 指定ドキュメントの疎テキスト読み込みを行います。
pub fn load_sparse_doc(doc_id: usize, line_count: Option<usize>) {
    let doc = get_or_create_doc(doc_id);
    doc.borrow_mut().load_sparse(line_count);
    PANES.with(|panes| {
        for session in panes.borrow().iter() {
            if session.borrow().doc_id == doc_id {
                session.borrow_mut().pending_tail = None;
            }
        }
    });
    redraw_doc(doc_id, None);
}

/// 保留状態を開始します。line_count が 0 のときは行数未確定（走査中）として扱われます。
#[allow(dead_code)]
pub fn load_pending(line_count: usize) {
    let Some(session) = session() else { return };
    let doc_id = session.borrow().doc_id;
    if doc_id != UNBOUND_DOC_ID {
        load_pending_doc(doc_id, line_count);
    } else {
        session
            .borrow()
            .document
            .borrow_mut()
            .load_pending(line_count);
        session.borrow_mut().pending_tail = None;
        changed(&session);
    }
}

/// 指定ドキュメントの保留状態を開始します。line_count が 0 のときは行数未確定（走査中）として扱われます。
#[allow(dead_code)]
pub fn load_pending_doc(doc_id: usize, line_count: usize) {
    let doc = get_or_create_doc(doc_id);
    doc.borrow_mut().load_pending(line_count);
    PANES.with(|panes| {
        for session in panes.borrow().iter() {
            if session.borrow().doc_id == doc_id {
                session.borrow_mut().pending_tail = None;
            }
        }
    });
    redraw_doc(doc_id, None);
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
    let doc_id = session.borrow().doc_id;
    if doc_id != UNBOUND_DOC_ID {
        let doc = get_or_create_doc(doc_id);
        doc.borrow_mut().counting = false;

        PANES.with(|panes| {
            for s in panes.borrow().iter() {
                if s.borrow().doc_id == doc_id {
                    let pending_tail = s.borrow_mut().pending_tail.take();
                    s.borrow_mut().edit(|editor| {
                        apply_line_count(editor, pending_tail, count);
                    });
                    redraw(s);
                }
            }
        });
    } else {
        let pending_tail = session.borrow_mut().pending_tail.take();
        session.borrow_mut().edit(|editor| {
            apply_line_count(editor, pending_tail, count);
        });
        redraw(&session);
    }
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
    let doc_id = session.borrow().doc_id;
    let read_lines: Vec<_> = lines.iter().map(|line| document::read_line(line)).collect();

    // 共有の Document に行データを投入（同一 doc_id の全ペインへ即座に反映される）
    session
        .borrow()
        .document
        .borrow_mut()
        .feed(from, read_lines);

    {
        let borrowed = session.borrow();
        let mut doc = borrowed.document.borrow_mut();
        if doc.resident_lines() > RESIDENT_LIMIT {
            let mut keep_ranges = Vec::new();
            let mut pinned: Vec<usize> = Vec::new();

            if doc_id != UNBOUND_DOC_ID {
                PANES.with(|panes| {
                    for s in panes.borrow().iter() {
                        let b = s.borrow();
                        if b.doc_id == doc_id {
                            let drawn = b.view.drawn();
                            keep_ranges.push(
                                drawn.start.saturating_sub(RESIDENT_KEEP)
                                    ..drawn.end + RESIDENT_KEEP,
                            );
                            for sel in b.cursors() {
                                pinned.push(sel.start().line);
                                pinned.push(sel.end().line);
                            }
                        }
                    }
                });
            }

            if keep_ranges.is_empty() {
                let drawn = borrowed.view.drawn();
                keep_ranges
                    .push(drawn.start.saturating_sub(RESIDENT_KEEP)..drawn.end + RESIDENT_KEEP);
                for sel in borrowed.cursors() {
                    pinned.push(sel.start().line);
                    pinned.push(sel.end().line);
                }
            }

            doc.evict_far(&keep_ranges, &pinned);
        }
    }

    // 届いた行が画面に見えるペインを再描画
    let target_sessions = if doc_id != UNBOUND_DOC_ID {
        PANES.with(|panes| {
            panes
                .borrow()
                .iter()
                .filter(|s| s.borrow().doc_id == doc_id)
                .cloned()
                .collect::<Vec<_>>()
        })
    } else {
        vec![session.clone()]
    };

    for s in target_sessions {
        let (visible, follows_caret) = {
            let borrowed = s.borrow();
            let drawn = borrowed.view.drawn();
            (
                from < drawn.end && from + lines.len() > drawn.start,
                drawn.contains(&borrowed.primary().head.line),
            )
        };
        if visible {
            s.borrow().view.invalidate();
            if follows_caret {
                redraw(&s);
            } else {
                scrolled(&s);
            }
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

/// 他ペインの同一ドキュメント（スライス）へ、行数の増減に伴うキャレット追従シフトを適用して画面を同期します。
/// テキスト実体は Rc<RefCell<Document>> で共有されているため、編集自体は即座に反映されています。
pub fn apply_flush_to_other_panes(origin_pane: usize, batch: &FlushBatch) {
    if batch.edits.is_empty() {
        return;
    }

    let other_sessions = PANES.with(|panes| {
        let panes = panes.borrow();
        let origin_doc_id = panes
            .iter()
            .find(|s| s.borrow().pane == origin_pane)
            .map(|s| s.borrow().doc_id);
        match origin_doc_id {
            Some(id) if id != UNBOUND_DOC_ID => panes
                .iter()
                .filter(|s| s.borrow().pane != origin_pane && s.borrow().doc_id == id)
                .cloned()
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        }
    });

    if other_sessions.is_empty() {
        return;
    }

    for session in other_sessions {
        {
            let document = session.borrow().document.clone();
            let doc = document.borrow();
            let mut session_mut = session.borrow_mut();
            for edit in &batch.edits {
                let delta = edit.lines.len() as isize - (edit.to as isize - edit.from as isize);
                if delta != 0 {
                    for cursor in &mut session_mut.cursors {
                        if cursor.sel.anchor.line >= edit.to {
                            cursor.sel.anchor.line =
                                (cursor.sel.anchor.line as isize + delta).max(0) as usize;
                        } else if cursor.sel.anchor.line >= edit.from {
                            cursor.sel.anchor.line = (edit.from
                                + edit.lines.len().saturating_sub(1))
                            .min(doc.text().line_count().saturating_sub(1));
                        }
                        if cursor.sel.head.line >= edit.to {
                            cursor.sel.head.line =
                                (cursor.sel.head.line as isize + delta).max(0) as usize;
                        } else if cursor.sel.head.line >= edit.from {
                            cursor.sel.head.line = (edit.from + edit.lines.len().saturating_sub(1))
                                .min(doc.text().line_count().saturating_sub(1));
                        }
                        cursor.sel.anchor = doc.text().clamp(cursor.sel.anchor);
                        cursor.sel.head = doc.text().clamp(cursor.sel.head);
                    }
                }
            }
        }
        session.borrow().view.invalidate();
        scrolled(&session);
    }
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

/// 指定ドキュメントの変更行を取得します。
pub fn doc_modified_lines(doc_id: usize) -> Vec<usize> {
    let doc = get_or_create_doc(doc_id);
    let lines = doc.borrow().modified_lines();
    lines
}

/// 指定ドキュメントを開いている全ペインの変更行マーカーを設定します。
pub fn set_doc_modified_lines(doc_id: usize, lines: Vec<usize>) {
    let doc = get_or_create_doc(doc_id);
    doc.borrow_mut().set_modified_lines(lines);
    redraw_doc(doc_id, None);
}

/// 指定ドキュメントを開いている全ペインの全行を変更行としてマークします。
pub fn mark_doc_all_modified(doc_id: usize) {
    let doc = get_or_create_doc(doc_id);
    let count = doc.borrow().text().line_count();
    doc.borrow_mut().set_modified_lines((0..count).collect());
    redraw_doc(doc_id, None);
}

/// 指定ドキュメントを開いている全ペインへ、テキスト全体（無題ドラフト等）を読み込みます。
pub fn load_doc_contents(doc_id: usize, text: &str) {
    let parsed = document::read(text);
    let doc = get_or_create_doc(doc_id);
    doc.borrow_mut().load(parsed);

    PANES.with(|panes| {
        for session in panes.borrow().iter() {
            if session.borrow().doc_id == doc_id {
                session.borrow_mut().pending_tail = None;
            }
        }
    });
    redraw_doc(doc_id, None);
}

/// スプライス（行の挿入・削除）に伴うカーソルの追従シフト補正を行います。
pub(super) fn shift_cursors_for_splices(
    cursors: &mut [UnifiedCursor],
    doc: &Document,
    splices: &[crate::framework::SpliceEdit],
) {
    for splice in splices {
        let delta = splice.lines.len() as isize - (splice.to as isize - splice.from as isize);
        if delta != 0 {
            for cursor in cursors.iter_mut() {
                if cursor.sel.anchor.line >= splice.to {
                    cursor.sel.anchor.line =
                        (cursor.sel.anchor.line as isize + delta).max(0) as usize;
                } else if cursor.sel.anchor.line >= splice.from {
                    cursor.sel.anchor.line = (splice.from + splice.lines.len().saturating_sub(1))
                        .min(doc.text().line_count().saturating_sub(1));
                }
                if cursor.sel.head.line >= splice.to {
                    cursor.sel.head.line = (cursor.sel.head.line as isize + delta).max(0) as usize;
                } else if cursor.sel.head.line >= splice.from {
                    cursor.sel.head.line = (splice.from + splice.lines.len().saturating_sub(1))
                        .min(doc.text().line_count().saturating_sub(1));
                }
                cursor.sel.anchor = doc.text().clamp(cursor.sel.anchor);
                cursor.sel.head = doc.text().clamp(cursor.sel.head);
            }
        }
    }
}

/// 文書の本体が巻き戻ったのに合わせる: 該当ドキュメントを開いているセッションへ
/// テキスト差分を適用し、Undo発生元ペインのカーソルを復元、他ペインはスプライス追従のみを行う。
pub fn apply_restored(
    doc_id: usize,
    origin_pane: Option<usize>,
    state: &str,
    touched_from: usize,
    line_count: usize,
    splices: &[crate::framework::SpliceEdit],
) {
    let sessions = PANES.with(|panes| panes.borrow().clone());
    let doc = get_or_create_doc(doc_id);

    // 1. 文書本体のモデル層を更新
    doc.borrow_mut()
        .apply_restored(touched_from, line_count, splices);

    // 2. セッション層のカーソル・表示を更新
    let mut origin_restored = false;
    for s in &sessions {
        if s.borrow().doc_id == doc_id {
            let is_origin = match origin_pane {
                Some(origin) => s.borrow().pane == origin,
                None => !origin_restored,
            };

            if is_origin {
                s.borrow_mut().edit(|editor| {
                    editor.restore_state(state);
                });
                origin_restored = true;
            } else {
                // 他ペインのカーソルは相手の state で上書きせず、
                // 差分行数に応じた位置補正（シフト）のみ行って独立カーソルを維持
                let mut session_mut = s.borrow_mut();
                let doc_ref = session_mut.document.clone();
                let doc_b = doc_ref.borrow();
                shift_cursors_for_splices(&mut session_mut.cursors, &doc_b, splices);
            }
            s.borrow().view.invalidate();
            scrolled(s);
        }
    }

    redraw_doc(doc_id, origin_pane.or_else(|| Some(FOCUSED.get())));
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

    /// 【1 Document : N View 分割ビュー検証テスト】
    /// 同一 doc_id を開く複数のペイン（分割ビュー）において、単一の Document Model (Rc<RefCell<Document>>)
    /// が共有され、先頭行と末尾行が同一のスパース配列内に正しく共存することを検証する。
    #[test]
    fn split_view_shares_document_model_in_sparse_array() {
        let doc_id = 42;
        let doc = get_or_create_doc(doc_id);

        // 1,000,000 行の巨大ファイルとして初期化
        doc.borrow_mut().load_sparse(Some(1_000_000));

        // ペイン 0 が文書先頭（0..100）を取り寄せ
        let pane0_lines: Vec<_> = (0..100)
            .map(|i| document::read_line(&format!("pane0 line {i}")))
            .collect();
        doc.borrow_mut().feed(0, pane0_lines);

        // ペイン 1 が文書末尾（900_000..900_100）を取り寄せ
        let pane1_lines: Vec<_> = (900_000..900_100)
            .map(|i| document::read_line(&format!("pane1 line {i}")))
            .collect();
        doc.borrow_mut().feed(900_000, pane1_lines);

        let borrowed = doc.borrow();
        // 行 0 も行 900_000 も同じ Document 内で resident
        assert!(!borrowed.text().is_absent(0));
        assert!(!borrowed.text().is_absent(900_000));
        // 中間の未読込行は absent（メモリを浪費しない）
        assert!(borrowed.text().is_absent(500_000));
    }

    /// 【分割ビュー即時同期テスト】
    /// 同一 doc_id を開く別ペインへ、行数増減に伴うキャレット追従シフトが適用され、
    /// テキスト変更が同一の Document を通じて即時に共有されることを保証する。
    #[test]
    fn split_view_cursor_shifts_and_shares_edits() {
        let doc_id = 99;
        let doc = get_or_create_doc(doc_id);
        doc.borrow_mut().load_sparse(Some(10));

        let lines: Vec<_> = (0..10)
            .map(|i| document::read_line(&format!("line {i}")))
            .collect();
        doc.borrow_mut().feed(0, lines);

        // 編集前の行数は 10
        assert_eq!(doc.borrow().text().line_count(), 10);

        // 行 1 を 3 行に置換（delta = +2）
        let edited_lines = vec![
            "line 1 edited".to_string(),
            "new line 1b".to_string(),
            "new line 1c".to_string(),
        ];
        let source_lines: Vec<_> = edited_lines
            .into_iter()
            .map(crate::structure::text::SourceLine::Plain)
            .collect();
        doc.borrow_mut().text.replace_external(1, 2, source_lines);
        doc.borrow_mut().mark_lines_modified(1, 2, 3);

        // 全体行数が 12 行になっていること
        assert_eq!(doc.borrow().text().line_count(), 12);
        assert_eq!(doc.borrow().text().raw_line(1), Some("line 1 edited"));
        assert_eq!(doc.borrow().text().raw_line(2), Some("new line 1b"));
        assert_eq!(doc.borrow().text().raw_line(3), Some("new line 1c"));
        assert_eq!(doc.borrow().text().raw_line(4), Some("line 2"));

        let modified = doc.borrow().modified_lines();
        assert!(modified.contains(&1));
        assert!(modified.contains(&2));
        assert!(modified.contains(&3));
    }

    /// 【分割ビュー白飛び防止テスト】
    /// 既存のドキュメントに新しいペインをバインドした際、同一の Document への Rc 参照が渡され、
    /// 新規ペインが総行数1の空文書（真っ白）にならず、元の内容を0msで即座に保持していることを検証する。
    #[test]
    fn split_view_bind_doc_shares_existing_document_without_blank() {
        let doc_id = 77;
        let doc = get_or_create_doc(doc_id);
        doc.borrow_mut().load_sparse(Some(50));

        let original_lines: Vec<_> = (0..50)
            .map(|i| document::read_line(&format!("content line {i}")))
            .collect();
        doc.borrow_mut().feed(0, original_lines);

        assert_eq!(doc.borrow().text().line_count(), 50);
        assert!(!doc.borrow().text().is_absent(0));

        // 別ペインが同一 doc_id を取得
        let shared_doc = get_or_create_doc(doc_id);
        assert_eq!(shared_doc.borrow().text().line_count(), 50);
        assert!(!shared_doc.borrow().text().is_absent(0));
        assert_eq!(
            shared_doc.borrow().text().raw_line(0),
            Some("content line 0")
        );
        assert_eq!(
            shared_doc.borrow().text().raw_line(49),
            Some("content line 49")
        );
    }

    /// 【下書き復元時の白飛び防止テスト】
    /// ペインがまだマウントされていない段階で load_doc_contents が呼ばれても、
    /// DOCUMENTS にパース済み行が保存され、
    /// 後からペインがバインドされた際に空文書（1行真っ白）にならず全文が即座に復元されることを検証する。
    #[test]
    fn draft_restore_before_pane_mount_persists_in_document_and_binds_correctly() {
        let doc_id = 999;
        let sample_text = "line 1\nline 2\nline 3\nline 4\nline 5";

        // ペインマウント前に load_doc_contents が呼ばれる
        load_doc_contents(doc_id, sample_text);

        // Document に状態が保持されていること
        let doc = get_or_create_doc(doc_id);
        let borrowed = doc.borrow();
        assert_eq!(borrowed.text().line_count(), 5);
        assert_eq!(borrowed.text().raw_line(0), Some("line 1"));
        assert_eq!(borrowed.text().raw_line(4), Some("line 5"));
    }

    /// 【Undo後の白飛び防止テスト】
    /// Undo 時に touched_from 以降が一旦 Absent にリセットされた後、
    /// 更新された known_revision に基づいて再フェッチされた行が正常にフィードされ、
    /// 画面が白飛びせず元通り復元されることを検証する。
    #[test]
    fn undo_apply_restored_resets_lines_and_allows_refeed() {
        let doc_id = 888;
        let doc = get_or_create_doc(doc_id);
        doc.borrow_mut().load_sparse(Some(20));

        let initial_lines: Vec<_> = (0..20)
            .map(|i| document::read_line(&format!("initial line {i}")))
            .collect();
        doc.borrow_mut().feed(0, initial_lines);

        // 編集後の状態: revision = 2
        doc.borrow_mut().known_revision = 2;
        assert_eq!(doc.borrow().text().line_count(), 20);
        assert_eq!(doc.borrow().text().raw_line(10), Some("initial line 10"));

        // Undo 実行: revision は 1 に戻り、行 10 以降が巻き戻し対象
        let restored_revision = 1;
        doc.borrow_mut().known_revision = restored_revision;

        apply_restored(doc_id, None, "0.0-0.0", 10, 20, &[]);

        // 行 0..10 はそのまま保持され、行 10..20 は Absent になっている（フォールバック動作）
        assert_eq!(doc.borrow().text().raw_line(0), Some("initial line 0"));
        assert_eq!(doc.borrow().text().raw_line(9), Some("initial line 9"));
        assert!(doc.borrow().text().is_absent(10));
        assert!(doc.borrow().text().is_absent(19));

        // バックエンドから revision 1 の巻き戻し後行データが再フェッチされて feed される
        let restored_lines: Vec<_> = (10..20)
            .map(|i| document::read_line(&format!("restored line {i}")))
            .collect();
        doc.borrow_mut().feed(10, restored_lines);

        // 全行が正常に復元されていること
        assert_eq!(doc.borrow().text().raw_line(10), Some("restored line 10"));
        assert_eq!(doc.borrow().text().raw_line(19), Some("restored line 19"));
        assert!(!doc.borrow().text().is_absent(10));
    }

    /// 【スプライス差分直接適用テスト（方向性A）】
    /// 文書エンジンから届いた SpliceEdit（行置き換え差分）を適用した場合、
    /// 手元の行が Absent（未着）にならず、再フェッチも待たずに手元で即座に元通り更新されることを検証する。
    #[test]
    fn undo_with_splices_updates_directly_without_blanking() {
        use crate::framework::SpliceEdit;
        let doc_id = 777;
        let doc = get_or_create_doc(doc_id);
        doc.borrow_mut().load_sparse(Some(15));

        let initial_lines: Vec<_> = (0..15)
            .map(|i| document::read_line(&format!("original line {i}")))
            .collect();
        doc.borrow_mut().feed(0, initial_lines);

        // 編集: 行 11 を編集した状態
        doc.borrow_mut().text.replace_external(
            11,
            12,
            vec![crate::structure::text::SourceLine::Plain(
                "edited line 11".into(),
            )],
        );
        assert_eq!(doc.borrow().text().raw_line(11), Some("edited line 11"));

        // Undo: 文書エンジンから「行 11 を original line 11 に戻す」SpliceEdit が届く
        let splices = vec![SpliceEdit {
            from: 11,
            to: 12,
            lines: vec!["original line 11".to_string()],
        }];

        apply_restored(doc_id, None, "0.0-0.0", 11, 15, &splices);

        // 手元の行は Absent にならず、即座に元に戻っていること
        assert_eq!(doc.borrow().text().raw_line(11), Some("original line 11"));
        assert!(!doc.borrow().text().is_absent(11));
        assert_eq!(doc.borrow().text().raw_line(12), Some("original line 12"));
        assert_eq!(doc.borrow().text().first_absent(0), None);
    }

    /// 【連動グループと同一入力グループ Undo テスト（Step 22 検証）】
    /// 1. 同一ドキュメントを開く複数ペイン（スライス）が連動している時、それぞれのカーソルは独立して保持され、共有カーソルの実体は作られないこと。
    /// 2. `begin_group` / `end_group` の連動トランザクション内で複数カーソルによる編集を順次直列化して適用した時、
    ///    同一の Document Model に対して同一の入力グループ（同一 group ID）として記録され、1つの FlushBatch にまとまること。
    /// 3. 文書エンジンから 1 回の Undo（apply_restored）が届いた時、連動していた両方のペインの編集が一度に元に戻ること。
    #[test]
    fn linked_panes_share_single_undo_group_and_restore_together() {
        use crate::framework::SpliceEdit;
        let doc_id = 888;
        let doc = get_or_create_doc(doc_id);
        doc.borrow_mut().load_sparse(Some(10));

        let initial_lines: Vec<_> = (0..10)
            .map(|i| document::read_line(&format!("line {i}")))
            .collect();
        doc.borrow_mut().feed(0, initial_lines);

        // 2 つのスライスのカーソル（独立して保持される。共有カーソル構造体は存在しない）
        let mut cursor_pane1 = vec![UnifiedCursor::caret(Pos::new(1, 0))];
        let mut cursor_pane2 = vec![UnifiedCursor::caret(Pos::new(5, 0))];

        // 独立カーソルの検証
        assert_eq!(cursor_pane1[0].sel.head, Pos::new(1, 0));
        assert_eq!(cursor_pane2[0].sel.head, Pos::new(5, 0));

        // 連動トランザクション開始（同一グループ化）
        let was_grouping = doc.borrow_mut().begin_group();

        // ペイン 1 の編集
        {
            let mut editor1 = Editor {
                document: std::mem::take(&mut *doc.borrow_mut()),
                cursors: std::mem::take(&mut cursor_pane1),
            };
            editor1.insert_text("A");
            *doc.borrow_mut() = editor1.document;
            cursor_pane1 = editor1.cursors;
        }

        // ペイン 2 の編集
        {
            let mut editor2 = Editor {
                document: std::mem::take(&mut *doc.borrow_mut()),
                cursors: std::mem::take(&mut cursor_pane2),
            };
            editor2.insert_text("B");
            *doc.borrow_mut() = editor2.document;
            cursor_pane2 = editor2.cursors;
        }

        // 連動トランザクション終了
        doc.borrow_mut().end_group(was_grouping);

        // 編集内容が Document に反映されていること
        assert_eq!(document::write_line(doc.borrow().text().line(1)), "Aline 1");
        assert_eq!(document::write_line(doc.borrow().text().line(5)), "Bline 5");

        // 各ペインのカーソルが独立して正しく進んでいること
        assert_eq!(cursor_pane1[0].sel.head, Pos::new(1, 1));
        assert_eq!(cursor_pane2[0].sel.head, Pos::new(5, 1));

        // FlushBatch を取り出す（両方の編集が同一グループ ID で 1 つのバッチにまとまっていること）
        let mut editor_for_flush = Editor {
            document: std::mem::take(&mut *doc.borrow_mut()),
            cursors: vec![],
        };
        let flush = take_flush_of(&mut editor_for_flush).expect("flush batch should exist");
        *doc.borrow_mut() = editor_for_flush.document;

        assert_eq!(
            flush.edits.len(),
            2,
            "2箇所の編集が同一バッチにまとめられていること"
        );
        assert!(flush.group > 0);

        // 1 回の Undo（apply_restored）で両方の編集が一度に元に戻ること
        let splices = vec![
            SpliceEdit {
                from: 1,
                to: 2,
                lines: vec!["line 1".to_string()],
            },
            SpliceEdit {
                from: 5,
                to: 6,
                lines: vec!["line 5".to_string()],
            },
        ];

        apply_restored(doc_id, None, "1.0-1.0", 1, 10, &splices);

        // 両ペインの変更が 1 回の Undo で元通り復元されたこと
        assert_eq!(doc.borrow().text().raw_line(1), Some("line 1"));
        assert_eq!(doc.borrow().text().raw_line(5), Some("line 5"));
    }

    /// 【同一文書マルチペイン Undo 時の独立カーソル保護テスト】
    /// 相手ペインの Undo（apply_restored / shift_cursors_for_splices）が発生した際、
    /// 他ペインのカーソルが相手のカーソル位置に合流せず、自身のカーソル行が維持されること。
    /// かつ、Undo による行増減（splice delta）がある場合は正しく追従シフトされることを検証する。
    #[test]
    fn shift_cursors_for_splices_preserves_independent_cursor_of_other_pane() {
        use crate::framework::SpliceEdit;
        let doc_id = 7777;
        let doc = get_or_create_doc(doc_id);
        doc.borrow_mut().load_sparse(Some(20));

        let initial_lines: Vec<_> = (0..20)
            .map(|i| document::read_line(&format!("line {i}")))
            .collect();
        doc.borrow_mut().feed(0, initial_lines);

        // 他ペイン（ペイン 2）のカーソル: 行 10 に位置している
        let mut pane2_cursors = vec![UnifiedCursor::caret(Pos::new(10, 3))];

        // ケース 1: ペイン 1 の行 1 の変更が Undo された（行数増減なし delta = 0）
        let splices_same_count = vec![SpliceEdit {
            from: 1,
            to: 2,
            lines: vec!["line 1 original".to_string()],
        }];
        shift_cursors_for_splices(&mut pane2_cursors, &doc.borrow(), &splices_same_count);

        // ペイン 2 のカーソルは相手の Undo（行 1）に合流せず、行 10 のまま維持される
        assert_eq!(pane2_cursors[0].sel.head, Pos::new(10, 3));
        assert_eq!(pane2_cursors[0].sel.anchor, Pos::new(10, 3));

        // ケース 2: ペイン 1 の Undo により、上流の行が 2 行削除された（from: 1, to: 3 -> 1 行に縮小: delta = -1）
        let splices_shrink = vec![SpliceEdit {
            from: 1,
            to: 3,
            lines: vec!["line 1 single".to_string()],
        }];
        shift_cursors_for_splices(&mut pane2_cursors, &doc.borrow(), &splices_shrink);

        // ペイン 2 のカーソルは 1 行上に追従シフトして 行 9 になる（行 1 に合流することはない）
        assert_eq!(pane2_cursors[0].sel.head, Pos::new(9, 3));
        assert_eq!(pane2_cursors[0].sel.anchor, Pos::new(9, 3));
    }

    /// 【非連動時の linked_panes 単独性テスト】
    /// 連動していないペインに対して linked_panes を呼んだ場合、自身のみの 1 要素 Vec が返ること。
    #[test]
    fn linked_panes_isolated_when_not_linked() {
        let panes = linked_panes(999);
        assert_eq!(panes, vec![999]);
    }

    #[test]
    fn linked_typing_consecutive_chars_group_check() {
        let doc_id = 1111;
        let doc = get_or_create_doc(doc_id);
        doc.borrow_mut().load_sparse(Some(1));
        doc.borrow_mut().feed(0, vec![document::read_line("")]);

        let mut c1 = vec![UnifiedCursor::caret(Pos::new(0, 0))];
        let mut c2 = vec![UnifiedCursor::caret(Pos::new(0, 0))];

        // 1文字目 'a' (連動)
        let was_grouping = doc.borrow_mut().begin_group();
        {
            let mut ed1 = Editor {
                document: std::mem::take(&mut *doc.borrow_mut()),
                cursors: std::mem::take(&mut c1),
            };
            ed1.insert_text("a");
            *doc.borrow_mut() = ed1.document;
            c1 = ed1.cursors;
        }
        {
            let mut ed2 = Editor {
                document: std::mem::take(&mut *doc.borrow_mut()),
                cursors: std::mem::take(&mut c2),
            };
            ed2.insert_text("a");
            *doc.borrow_mut() = ed2.document;
            c2 = ed2.cursors;
        }
        doc.borrow_mut().end_group(was_grouping);

        let mut ed_flush1 = Editor {
            document: std::mem::take(&mut *doc.borrow_mut()),
            cursors: vec![],
        };
        let flush1 = take_flush_of(&mut ed_flush1).unwrap();
        *doc.borrow_mut() = ed_flush1.document;

        // 2文字目 'b' (連動)
        let was_grouping = doc.borrow_mut().begin_group();
        {
            let mut ed1 = Editor {
                document: std::mem::take(&mut *doc.borrow_mut()),
                cursors: std::mem::take(&mut c1),
            };
            ed1.insert_text("b");
            assert_eq!(ed1.cursors[0].sel.head, Pos::new(0, 2));
            *doc.borrow_mut() = ed1.document;
        }
        {
            let mut ed2 = Editor {
                document: std::mem::take(&mut *doc.borrow_mut()),
                cursors: std::mem::take(&mut c2),
            };
            ed2.insert_text("b");
            assert_eq!(ed2.cursors[0].sel.head, Pos::new(0, 2));
            *doc.borrow_mut() = ed2.document;
        }
        doc.borrow_mut().end_group(was_grouping);

        let mut ed_flush2 = Editor {
            document: std::mem::take(&mut *doc.borrow_mut()),
            cursors: vec![],
        };
        let flush2 = take_flush_of(&mut ed_flush2).unwrap();
        *doc.borrow_mut() = ed_flush2.document;

        assert_eq!(
            flush1.group, flush2.group,
            "consecutive typing across linked transaction must share group"
        );
    }
}
