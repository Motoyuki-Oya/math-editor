//! ペインごとに 1 つの編集セッション (V): 誰が画面上にあるか、誰がフォーカスを持っているか、および変更が画面とシェルにどのように到達するかの台帳。
//! 各セッションは Document ID を通じて共有の Document Model (M: Rc<RefCell<Editor>>) を参照します。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use web_sys::{HtmlElement, HtmlTextAreaElement};

use super::input;
use super::model::{Editor, Flush};
use super::search;
use crate::format::document;
use crate::view::document::{Caret, Overlay, View};

pub struct Session {
    /// このドキュメントが表示されているペインに名前を付けます。
    pub pane: usize,
    /// 現在表示している Document Model の ID。
    pub doc_id: usize,
    /// Document Model (M) への参照。
    pub editor: Rc<RefCell<Editor>>,
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
    /// 検索の一致一覧（手元にある場合の一括キャッシュ）。
    pub search_matches: Vec<search::Found>,
    /// 現在アクティブな一致のインデックス。
    pub search_index: usize,
    /// 行数の走査がまだ終わっていないか。終わるまで Ctrl+End は保留する。
    pub counting: bool,
    /// 走査完了を待っている Ctrl+End（値は shift）。確定したら跳ぶ。
    pub jump_end: Option<bool>,
    /// 行数未確定中にEOFから読んだ末尾行。仮位置と再配置用の元文字列。
    pending_tail: Option<(usize, Vec<String>)>,
}

/// ドキュメントが変更されたペインで呼び出されます。呼び出し中に再び変更が起きてもよいよう、台帳の借用の外で呼べる共有の参照で持ちます。
type OnChange = Rc<dyn Fn(usize)>;

thread_local! {
    /// 全ての開いているドキュメントの Model (M)。タブID（doc_id）ごとに 1 つ保持される。
    static DOCUMENTS: RefCell<HashMap<usize, Rc<RefCell<Editor>>>> = RefCell::new(HashMap::new());
    /// ペインごとに 1 つのセッション画面 (V)。分割ビューはリストを作成します。
    static PANES: RefCell<Vec<Rc<RefCell<Session>>>> = const { RefCell::new(Vec::new()) };
    /// 入力を行うペイン。
    static FOCUSED: Cell<usize> = const { Cell::new(0) };
    static NEXT_PANE: Cell<usize> = const { Cell::new(0) };
    static ON_CHANGE: RefCell<Option<OnChange>> = const { RefCell::new(None) };
}

/// 指定IDの Document Model を取得、無ければ新規作成して返します。
pub fn get_or_create_doc(id: usize) -> Rc<RefCell<Editor>> {
    DOCUMENTS.with(|docs| {
        docs.borrow_mut()
            .entry(id)
            .or_insert_with(|| Rc::new(RefCell::new(Editor::default())))
            .clone()
    })
}

/// タブがすべてのペインから破棄された際に、Document Model を解放します。
pub fn release_doc(id: usize) {
    DOCUMENTS.with(|docs| {
        docs.borrow_mut().remove(&id);
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
        borrowed.editor = doc;
        borrowed.preedit.clear();
        borrowed.search_from = None;
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
    let editor = get_or_create_doc(doc_id);
    let session = Rc::new(RefCell::new(Session {
        pane,
        doc_id,
        editor,
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
        search_matches: Vec::new(),
        search_index: 0,
        counting: false,
        jump_end: None,
        pending_tail: None,
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

/// 入力を「ペイン」に送信します。
pub fn focus_pane(pane: usize) {
    clear_linked();
    if pane_session(pane).is_some() {
        FOCUSED.set(pane);
    }
    focus();
}

/// イベントが発生したペインが、入力を受け取るペインです。
pub fn note_focus(session: &Rc<RefCell<Session>>) {
    FOCUSED.set(session.borrow().pane);
}

/// クリックしたペインを入力先にする。Alt+クリックで別ペインへ移った場合は、
/// 元のペインとクリック先を同じ入力グループへ入れる。通常クリックは解除する。
pub fn choose_pane(session: &Rc<RefCell<Session>>, add: bool) -> bool {
    let pane = session.borrow().pane;
    let was_linked = session.borrow().linked;
    let previous = FOCUSED.replace(pane);
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
        redraw(&target);
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
    borrowed.counting
        && borrowed.pending_tail.as_ref().is_some_and(|(from, lines)| {
            let line = borrowed.editor.borrow().primary().head.line;
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
        let editor = borrowed.editor.borrow();
        let drawn = borrowed.view.drawn();
        let Some(first) = editor.text().first_absent(drawn.start) else {
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

/// 検索欄の入力を可視範囲へ反映し、(現在の一致番号, 総一致件数) を返す。
pub fn preview_search(query: &str, options: search::SearchOptions) -> (usize, usize) {
    let Some(session) = session() else {
        return (0, 0);
    };
    let (cur, total) = {
        let mut borrowed = session.borrow_mut();
        borrowed.preview_query = query.to_string();
        borrowed.preview_options = options;
        refresh_preview(&mut borrowed);
        let editor_rc = borrowed.editor.clone();
        if query.is_empty() {
            borrowed.search_matches.clear();
            borrowed.search_index = 0;
            (0, 0)
        } else {
            let is_full = editor_rc.borrow().text().absent_lines() == 0;
            if is_full {
                let matches = {
                    let editor = editor_rc.borrow();
                    search::find_all(editor.text(), query, options, borrowed.preview_file_size)
                };
                let total = matches.len();
                if total == 0 {
                    borrowed.search_matches = matches;
                    borrowed.search_index = 0;
                    (0, 0)
                } else {
                    let cur_key = {
                        let editor = editor_rc.borrow();
                        search::key_at(editor.primary().start(), editor.nested_cursor())
                    };
                    let idx = matches
                        .iter()
                        .position(|m| m.place.start() >= cur_key)
                        .unwrap_or(0);
                    borrowed.search_matches = matches;
                    borrowed.search_index = idx;
                    (idx + 1, total)
                }
            } else {
                borrowed.search_matches.clear();
                borrowed.search_index = 0;
                (0, 0)
            }
        }
    };
    redraw_preview_overlay(&session);
    (cur, total)
}

pub fn clear_search_preview() {
    let Some(session) = session() else { return };
    {
        let mut borrowed = session.borrow_mut();
        borrowed.preview_query.clear();
        borrowed.preview_found.clear();
        borrowed.search_matches.clear();
        borrowed.search_index = 0;
    }
    redraw_preview_overlay(&session);
}

fn redraw_preview_overlay(session: &Rc<RefCell<Session>>) {
    let session = session.borrow();
    let editor = session.editor.borrow();
    let caret = caret_of(&session, &editor);
    let carets = carets_of(&session, &editor);
    let highlights = preview_highlights(&session);
    let modified = editor.modified_lines();
    let sels = editor.sels();
    let line_count = editor.text().line_count();
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
            show_numbers: !session.counting,
        },
    );
}

fn refresh_preview(session: &mut Session) {
    if session.preview_query.is_empty() {
        session.preview_found.clear();
        return;
    }
    let editor = session.editor.borrow();
    session.preview_found = search::find_range(
        editor.text(),
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
    {
        let mut borrowed = session.borrow_mut();
        borrowed.search_from = None;
        borrowed.search_matches.clear();
        borrowed.search_index = 0;
    }

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
            scrolled(&s);
        }
    }
}

/// 描き直してキャレットの行を見せ、隠しの入力欄をキャレットの場所についていかせます (IME の候補窓がそこに出ます)。
pub fn redraw(session: &Rc<RefCell<Session>>) {
    {
        let session = session.borrow();
        let editor = session.editor.borrow();
        let caret = caret_of(&session, &editor);
        let carets = carets_of(&session, &editor);
        let highlights = preview_highlights(&session);
        let modified = editor.modified_lines();
        let sels = editor.sels();
        session.view.draw(
            editor.text(),
            &Overlay {
                sels: &sels,
                highlights: &highlights,
                modified: &modified,
                primary: &caret,
                carets: &carets,
                focused: session.focused,
                linked: session.linked,
                show_numbers: !session.counting,
            },
        );
        if let Some(rect) = session.view.reveal(&caret) {
            input::follow_caret(&session.textarea, rect);
        }
    }
    // Ctrl+End などで窓が移った場合も、移動後の drawn 範囲を検索する。
    refresh_preview(&mut session.borrow_mut());
    redraw_preview_overlay(session);
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

/// ビューがスクロールされた後に再度描画するため、表示された行がページに配置されます。 redraw とは異なり、これはユーザーがスクロールしたビューを残し、キャレットに移動しません。
pub fn scrolled(session: &Rc<RefCell<Session>>) {
    {
        let session = session.borrow();
        let editor = session.editor.borrow();
        let caret = caret_of(&session, &editor);
        let carets = carets_of(&session, &editor);
        let highlights = preview_highlights(&session);
        let modified = editor.modified_lines();
        let sels = editor.sels();
        session.view.repaint(
            editor.text(),
            &Overlay {
                sels: &sels,
                highlights: &highlights,
                modified: &modified,
                primary: &caret,
                carets: &carets,
                focused: session.focused,
                linked: session.linked,
                show_numbers: !session.counting,
            },
        );
    }
    // repaint で新しい窓が確定してから、その窓を検索して重ねだけを更新する。
    // 先に検索すると、1つ前の drawn 範囲をハイライトしてしまう。
    refresh_preview(&mut session.borrow_mut());
    redraw_preview_overlay(session);
    request_missing(session);
}

/// 1 つのキャレットで両方のケースを説明するため、描画に選択するモードはありません。
fn caret_of<'a>(session: &'a Session, editor: &'a Editor) -> Caret<'a> {
    Caret {
        at: editor.primary().head,
        inside: editor.nested_cursor(),
        composing: (!session.preedit.is_empty()).then_some(session.preedit.as_str()),
    }
}

fn carets_of<'a>(session: &'a Session, editor: &'a Editor) -> Vec<Caret<'a>> {
    let last = editor.cursors().len().saturating_sub(1);
    editor
        .cursors()
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
pub fn load(text: &str) {
    let Some(session) = session() else { return };
    session
        .borrow()
        .editor
        .borrow_mut()
        .load_contents(document::read(text));
    changed(&session);
}

/// 行数だけ分かっている文書を出します。行は見えた場所から取り寄せられます。
/// 行数は走査中の途中値なので、確定は [set_line_count] で届く。
pub fn load_pending(line_count: usize) {
    let Some(session) = session() else { return };
    {
        let borrowed = session.borrow();
        borrowed.editor.borrow_mut().load_pending(line_count);
    }
    {
        let mut borrowed = session.borrow_mut();
        borrowed.counting = true;
        borrowed.jump_end = None;
        borrowed.pending_tail = None;
    }
    changed(&session);
}

/// 行数未確定でも、EOFから届いた末尾ウィンドウを仮の末尾へ表示する。
pub fn show_tail(pane: usize, lines: &[String]) {
    let Some(session) = pane_session(pane) else {
        return;
    };
    let from = {
        let borrowed = session.borrow();
        let mut editor = borrowed.editor.borrow_mut();
        let count = editor.text().line_count();
        let from = count.saturating_sub(lines.len());
        editor.forget_range(from..count);
        editor.feed(
            from,
            lines.iter().map(|line| document::read_line(line)).collect(),
        );
        let last = count.saturating_sub(1);
        let col = editor.text().line_len(last);
        editor.set_caret(crate::structure::text::Pos::new(last, col));
        from
    };
    {
        let mut borrowed = session.borrow_mut();
        borrowed.pending_tail = borrowed.counting.then(|| (from, lines.to_vec()));
    }
    redraw(&session);
}

fn apply_line_count(
    editor: &mut Editor,
    pending_tail: Option<(usize, Vec<String>)>,
    jump_end: Option<bool>,
    count: usize,
) {
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
        editor.set_caret(crate::structure::text::Pos::new(last, col));
    } else if let Some(shift) = jump_end {
        editor.move_document_edge(true, shift);
    }
}

/// 走査で確定した行数をペインの文書へ合わせます。仮表示した末尾は、
/// 確定した絶対行番号へ付け替えます。
pub fn set_line_count(pane: usize, count: usize) {
    let Some(session) = pane_session(pane) else {
        return;
    };
    let (pending_tail, jump_end) = {
        let mut borrowed = session.borrow_mut();
        let pending_tail = borrowed.pending_tail.take();
        let jump_end = borrowed.jump_end.take();
        borrowed.counting = false;
        (pending_tail, jump_end)
    };
    {
        let borrowed = session.borrow();
        apply_line_count(
            &mut borrowed.editor.borrow_mut(),
            pending_tail,
            jump_end,
            count,
        );
    }
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
    session.borrow().editor.borrow_mut().feed(
        from,
        lines.iter().map(|line| document::read_line(line)).collect(),
    );
    {
        let borrowed = session.borrow();
        let mut editor = borrowed.editor.borrow_mut();
        if editor.resident_lines() > RESIDENT_LIMIT {
            let drawn = borrowed.view.drawn();
            editor.evict_far(drawn.start.saturating_sub(RESIDENT_KEEP)..drawn.end + RESIDENT_KEEP);
        }
    }
    // 描き直すのは届いた行が画面に見えるときだけ。
    let (visible, follows_caret) = {
        let borrowed = session.borrow();
        let editor = borrowed.editor.borrow();
        let drawn = borrowed.view.drawn();
        (
            from < drawn.end && from + lines.len() > drawn.start,
            drawn.contains(&editor.primary().head.line),
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
    let borrowed = session.borrow();
    let mut editor = borrowed.editor.borrow_mut();
    take_flush_of(&mut editor)
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

pub fn stats() -> (usize, usize) {
    session()
        .map(|session| {
            let borrowed = session.borrow();
            let (characters, lines) = borrowed.editor.borrow().text().stats();
            (characters, if borrowed.counting { 0 } else { lines })
        })
        .unwrap_or((0, 1))
}

/// 入力を受けるペインの文書が手元に全部あるか。検索や置換が文書の本体の
/// 走査を要るかの見分け。
pub fn fully_resident() -> bool {
    session().is_some_and(|session| session.borrow().editor.borrow().text().absent_lines() == 0)
}

/// 保存などで文書がファイルと一致した際、変更行マーカーをクリアします。
pub fn clear_modified(pane: usize) {
    if let Some(session) = pane_session(pane) {
        let doc_id = session.borrow().doc_id;
        session.borrow().editor.borrow_mut().clear_modified();
        redraw_doc(doc_id, None);
    }
}
