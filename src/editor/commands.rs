//! エディターのコマンド: 入力、IME、クリップボード、パレット、検索と置換がドキュメントに対して行うこと。

use std::cell::RefCell;
use std::rc::Rc;

use web_sys::InputEvent;

use super::clipboard::{self, Clip};
use super::model::Did;
use super::search::{self, Place, SearchOptions};
use super::session::{changed, focus, redraw, session, Session};
use super::trigger;
use crate::structure::ast::Node;

pub fn on_input(session: &Rc<RefCell<Session>>, event: InputEvent) {
    let textarea = session.borrow().textarea.clone();
    let text = textarea.value();
    if session.borrow().composing {
        // まだ作成中。 `compositionupdate` は完了するまで描画します。
        update_composition(session, &text);
        event.stop_propagation();
        return;
    }
    textarea.set_value("");
    if text.is_empty() {
        return;
    }
    insert_text(session, &text);
}

/// コミットされる前に IME が何を構成しているかを表示します。
pub fn update_composition(session: &Rc<RefCell<Session>>, event_text: &str) {
    let textarea_text = session.borrow().textarea.value();
    session.borrow_mut().preedit = composition_text(event_text, textarea_text);
    redraw(session);
}

pub fn commit_composition(session: &Rc<RefCell<Session>>, event_text: &str) {
    let textarea = session.borrow().textarea.clone();
    let text = composition_text(event_text, textarea.value());
    textarea.set_value("");
    session.borrow_mut().preedit.clear();
    if !text.is_empty() {
        insert_text(session, &text);
    } else {
        redraw(session);
    }
}

fn composition_text(event_text: &str, textarea_text: String) -> String {
    if textarea_text.is_empty() {
        event_text.to_string()
    } else {
        textarea_text
    }
}

pub fn insert_text(session: &Rc<RefCell<Session>>, text: &str) {
    // 単一の文字で構造を開始することもできます。
    let mut chars = text.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if trigger::type_char(session, c) {
            return;
        }
    }
    // ドキュメントからコピーされた部分は、元の形状で戻ります。それ以外のテキストは、そのままの文字です。
    match clipboard::pasted(text) {
        Some(clip) => session.borrow_mut().editor.insert_clip(&clip),
        None => session.borrow_mut().editor.insert_text(text),
    };
    changed(session);
}

/// キャレットにアイランドを配置し、編集を開始します。
pub fn insert_structure() {
    let Some(session) = session() else { return };
    session.borrow_mut().editor.start_structure();
    focus();
    changed(&session);
}

/// パレットから構造をキャレット位置へ配置し、その編集スロットへ入ります。
pub fn annotate(upper: bool) {
    let Some(session) = session() else { return };
    let did = session.borrow_mut().editor.annotate(upper);
    focus();
    match did {
        Did::Changed => changed(&session),
        Did::Moved => redraw(&session),
        Did::Nothing => {}
    }
}

pub fn insert_node(node: Node) {
    let Some(session) = session() else { return };
    {
        // 構造の配置と編集開始は 1 つの操作なので、1 回元に戻すと両方を戻します。
        let mut borrowed = session.borrow_mut();
        borrowed.editor.one_step(|editor| {
            if editor.nested_cursor().is_none() {
                editor.start_structure();
            }
            editor.insert_node(node);
        });
    }
    focus();
    changed(&session);
}

/// キャレットがある場所すべてを選択します。キャレットが含まれる構造の行、または全体文書。システム独自の全選択アイテムは、テキストではなく非表示の入力要素に到達するため、これは独自のアイテムです。
pub fn select_all() {
    let Some(session) = session() else { return };
    session.borrow_mut().editor.select_all();
    focus();
    redraw(&session);
}

/// 選択によってクリップボードに置かれるテキストは、通常のテキストです。部分自体は保存されるため、エディタに貼り付けて戻すと、表記がファイルから離れることなく形状が維持されます。
///
/// 「なし」は、何も選択されていないことを意味します。空の構造など、テキストが空の選択範囲は依然として選択範囲であり、切り取ることができます。
pub fn selected_text(session: &Rc<RefCell<Session>>) -> Option<String> {
    let borrowed = session.borrow();
    // 構造内の選択範囲は、構造のその部分をコピーします。クリップボードはどちらの方法でも同じです。
    if let Some(row) = borrowed.editor.nested_selection() {
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

/// まだ届いていない行を含む選択。手元では空に見えているだけなので、中身は
/// 文書の本体に組み立ててもらう。端の行の切り出しと、構造を含む行の読み下し
/// （手元にある分）はここで作る。
pub struct FarCopy {
    pub from_line: usize,
    /// 端の行の切り出し。`None` は行を丸ごと。
    pub first: Option<String>,
    pub to_line: usize,
    pub last: Option<String>,
    /// 素の文字列のままではない行の、読み下したテキスト。
    pub overrides: Vec<(usize, String)>,
}

/// 選択がまだ届いていない行に触れていれば、本体でのコピーを依頼して `true`。
/// 手元に全部ある選択は `None` で、今までどおりその場でコピーされる。
pub fn request_far_copy(session: &Rc<RefCell<Session>>) -> bool {
    let (pane, copy) = {
        let borrowed = session.borrow();
        let Some(copy) = far_copy(&borrowed) else {
            return false;
        };
        (borrowed.pane, copy)
    };
    super::session::request_far_copy(pane, copy);
    true
}

fn far_copy(session: &Session) -> Option<FarCopy> {
    use crate::structure::plain;
    let editor = &session.editor;
    if editor.nested_selection().is_some() {
        return None;
    }
    let sel = editor.primary();
    if sel.is_caret() {
        return None;
    }
    let text = editor.text();
    let (from, to) = (sel.start(), sel.end());
    if text
        .first_absent(from.line)
        .is_none_or(|absent| absent > to.line)
    {
        return None;
    }
    // まだ届いていない行の長さは 0 なので、切り出しのある端の行は必ず手元にある。
    let first =
        (from.col > 0).then(|| plain::row(&text.line(from.line)[from.col..].to_vec()));
    let last = (to.col > 0 || !text.is_absent(to.line))
        .then(|| plain::row(&text.line(to.line)[..to.col].to_vec()));
    let mut overrides = Vec::new();
    for line in from.line..=to.line {
        if (line == from.line && first.is_some()) || (line == to.line && last.is_some()) {
            continue;
        }
        // 手元で編集や解析を経た行は、本体の記法の文字列と同じとは限らないので
        // 読み下したものを添える。素のままの行と届いていない行はそのまま。
        if !text.is_absent(line) && text.raw_line(line).is_none() {
            overrides.push((line, plain::row(&text.line(line).to_vec())));
        }
    }
    Some(FarCopy {
        from_line: from.line,
        first,
        to_line: to.line,
        last,
        overrides,
    })
}

/// 入れ子構造の編集を停止し、その直後にキャレットを残します。
pub fn leave_structure(session: &Rc<RefCell<Session>>) {
    session.borrow_mut().editor.leave_structure();
}

pub fn find_next(query: &str, options: SearchOptions, file_size: Option<usize>) -> bool {
    let Some(session) = session() else {
        return false;
    };
    let found = {
        let borrowed = session.borrow();
        let from = borrowed.search_from.clone().unwrap_or_else(|| {
            search::key_at(
                borrowed.editor.primary().end(),
                borrowed.editor.nested_cursor(),
            )
        });
        search::find_next(borrowed.editor.text(), query, options, file_size, from)
    };
    let Some(found) = found else {
        return false;
    };
    apply_found(&session, found);
    true
}

/// 一致を選択して見せ、「次を検索」がそこから続くようにします。
fn apply_found(session: &Rc<RefCell<Session>>, found: search::Found) {
    {
        let mut borrowed = session.borrow_mut();
        borrowed.search_from = Some(found.place.end());
        match found.place {
            // 構造内の一致がその中に表示されるため、どちらの方法でも見つかったものが選択されたものになります。
            Place::Text(sel) => borrowed.editor.set_sels(vec![sel]),
            Place::Inside { at, cursor } => {
                borrowed.editor.select_nested(at, cursor);
            }
        }
    }
    focus();
    redraw(session);
}

/// 文書の本体の走査で検索を続けるための出発点: 検索キーと行数。
pub fn far_search_start() -> Option<(search::Key, usize)> {
    let session = session()?;
    let borrowed = session.borrow();
    let from = borrowed.search_from.clone().unwrap_or_else(|| {
        search::key_at(
            borrowed.editor.primary().end(),
            borrowed.editor.nested_cursor(),
        )
    });
    Some((from, borrowed.editor.text().line_count()))
}

/// 本体の走査が見つけた素の行の一致へ跳びます。
pub fn apply_far_match(pane: usize, line: usize, start: usize, end: usize) -> bool {
    use crate::structure::text::Pos;
    let Some(session) = super::session::pane_session(pane) else {
        return false;
    };
    apply_found(
        &session,
        search::Found {
            place: Place::Text(crate::structure::text::Sel::range(
                Pos::new(line, start),
                Pos::new(line, end),
            )),
            groups: Vec::new(),
        },
    );
    true
}

/// 読み替えの要る行を手元で調べ、`after` より後の一致があれば選択します。
pub fn find_far_in_line(
    pane: usize,
    line: usize,
    query: &str,
    options: SearchOptions,
    file_size: Option<usize>,
    after: Option<&search::Key>,
) -> bool {
    let Some(session) = super::session::pane_session(pane) else {
        return false;
    };
    let found = {
        let borrowed = session.borrow();
        search::find_in_line(borrowed.editor.text(), line, query, options, file_size, after)
    };
    let Some(found) = found else {
        return false;
    };
    apply_found(&session, found);
    true
}

pub fn replace_all(
    query: &str,
    replacement: &str,
    options: SearchOptions,
    file_size: Option<usize>,
) -> usize {
    let Some(session) = session() else { return 0 };
    leave_structure(&session);
    let matches = {
        let borrowed = session.borrow();
        search::find_all(borrowed.editor.text(), query, options, file_size)
    };
    if matches.is_empty() {
        return 0;
    }
    {
        let mut borrowed = session.borrow_mut();
        // すべての置き換えが履歴の 1 ステップに入り、1 回の元に戻すで全部戻る。
        borrowed.editor.one_step(|editor| {
            // 後ろから前に置き換えると、以前の位置が有効になります。
            for found in matches.iter().rev() {
                let text = search::expand(&found.groups, replacement, options);
                match &found.place {
                    Place::Text(sel) => editor.replace_range_with(
                        sel.start(),
                        sel.end(),
                        search::replacement_nodes(&text),
                    ),
                    Place::Inside { at, cursor } => {
                        editor.replace_nested(*at, cursor.clone(), &text);
                    }
                }
            }
        });
        borrowed.editor.leave_structure();
    }
    changed(&session);
    matches.len()
}

#[cfg(test)]
mod tests {
    use super::composition_text;

    #[test]
    fn textarea_value_keeps_ime_preedit_inline_when_event_data_is_empty() {
        assert_eq!(composition_text("", "日本".into()), "日本");
        assert_eq!(composition_text("日本", "".into()), "日本");
        assert_eq!(composition_text("日", "日本".into()), "日本");
    }
}
