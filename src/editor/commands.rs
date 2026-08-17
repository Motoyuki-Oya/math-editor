//! エディターのコマンド: 入力、IME、クリップボード、パレット、検索と置換がドキュメントに対して行うこと。

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
        // まだ作成中。 `compositionupdate` は完了するまで描画します。
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
    // 単一の文字で数式を開始することもできます。
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
pub fn insert_math() {
    let Some(session) = session() else { return };
    session.borrow_mut().editor.insert_island();
    focus();
    changed(&session);
}

/// パレットから構造をキャレットの数式に配置し、キャレットが通常のテキスト内にあるときに数式を開始します。
pub fn insert_node(node: Node) {
    let Some(session) = session() else { return };
    {
        // 数式を開始してその中に構造を追加するのは 1 つのステップなので、1 回元に戻すとすべてが元に戻ります。
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

/// 数式の編集を停止し、その直後にキャレットを残します。
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
            // 構造内の一致がその中に表示されるため、どちらの方法でも見つかったものが選択されたものになります。
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
        // 後ろから前に置き換えると、以前の位置が有効になります。
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
