//! エディタ自体のキー: ドキュメント内のキーストロークが何を意味するか。
//!
//! テーブルが 1 つあります。構造体内のキーが何を意味するかは、モデルのビジネスであり、キーボードのビジネスではありません。そのため、その中にあるキャレットはここでは何も変更しません。エディター周辺でアプリケーションを駆動するキー (ファイル、タブ、ペイン、検索バー) は、`crate::app` の独自のテーブルに存在します。

use std::cell::RefCell;
use std::rc::Rc;

use web_sys::KeyboardEvent;

use super::model::Did;
use super::session::{self, changed, redraw, Session};

pub fn on_keydown(session: &Rc<RefCell<Session>>, event: KeyboardEvent) {
    if session.borrow().composing {
        return;
    }
    let key = event.key();
    let ctrl = event.ctrl_key() || event.meta_key();
    let shift = event.shift_key();
    if session::tail_locked(session) && matches!(key.as_str(), "Backspace" | "Delete" | "Enter") {
        event.prevent_default();
        return;
    }
    let linked_edit = if !ctrl {
        match key.as_str() {
            "Backspace" => apply_linked_edit(session, |editor| editor.backspace()),
            "Delete" => apply_linked_edit(session, |editor| editor.delete_forward()),
            "Enter" if !event.alt_key() => apply_linked_edit(session, |editor| editor.split_line()),
            _ => false,
        }
    } else {
        false
    };
    if linked_edit {
        event.prevent_default();
        return;
    }
    if key == "Escape" {
        session::clear_linked();
    }
    if ctrl && key == "End" && session.borrow().counting {
        let pane = session.borrow().pane;
        session::request_tail(pane);
        event.prevent_default();
        return;
    }
    let did = {
        let mut borrowed = session.borrow_mut();
        let editor = &mut borrowed.editor;
        match (ctrl, key.as_str()) {
            (_, "ArrowLeft") => editor.move_h(false, shift),
            (_, "ArrowRight") => editor.move_h(true, shift),
            (true, "ArrowUp") if !event.alt_key() && !shift => editor.annotate(true),
            (true, "ArrowDown") if !event.alt_key() && !shift => editor.annotate(false),
            (false, "ArrowUp") => editor.move_v(false, shift),
            (false, "ArrowDown") => editor.move_v(true, shift),
            // Alt+Up/Down は行を移動します。 Ctrl+Up/Down は上または下の共通欄を開きます。
            (true, "ArrowUp") | (true, "ArrowDown") => Did::Nothing,
            (false, "Home") => editor.move_line_edge(false, shift),
            (false, "End") => editor.move_line_edge(true, shift),
            (true, "Home") => editor.move_document_edge(false, shift),
            (true, "End") => editor.move_document_edge(true, shift),
            (false, "Backspace") => editor.backspace(),
            (false, "Delete") => editor.delete_forward(),
            // Alt+Enter または Ctrl+Enter でグリッドが 1 行ずつ拡大します。
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
            // 元に戻す、やり直し、およびクリップボードは、テキスト内でも構造内でも同様にウィンドウ ショートカットによって 1 回処理されます。
            (true, _) => Did::Nothing,
            // タブはテキスト内の列区切り文字であり、構造内の次のスロットです。
            (false, "Tab") => editor.tab(shift),
            // 印刷可能なキーは入力イベントとして到着し、IME もカバーします。
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

fn apply_linked_edit(
    origin: &Rc<RefCell<Session>>,
    edit: impl Fn(&mut super::model::Editor) -> Did,
) -> bool {
    let sessions = session::edit_sessions(origin);
    if sessions.len() < 2 {
        return false;
    }
    for session in sessions {
        let did = edit(&mut session.borrow_mut().editor);
        match did {
            Did::Changed => changed(&session),
            Did::Moved => redraw(&session),
            Did::Nothing => {}
        }
    }
    true
}
