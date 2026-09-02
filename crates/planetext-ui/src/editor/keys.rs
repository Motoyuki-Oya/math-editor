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
    if key == "Insert"
        && !ctrl
        && !shift
        && !event.alt_key()
        && (crate::settings::current().enable_overwrite_mode || session.borrow().overwrite_mode)
    {
        session::toggle_overwrite_mode(session);
        event.prevent_default();
        return;
    }
    if ctrl && key == "End" && session.borrow().counting {
        let pane = session.borrow().pane;
        session::request_tail(pane);
        event.prevent_default();
        return;
    }

    // ゴーストテキストが表示中の場合、Tab で確定挿入、Escape で消去
    if session::has_ghost_text(&session.borrow()) {
        if !ctrl && !event.alt_key() && key == "Tab" {
            if session::accept_suggestion(session) {
                event.prevent_default();
                return;
            }
        } else if key == "Escape" && session::clear_ghost_text(&mut session.borrow_mut()) {
            session::redraw(session);
            event.prevent_default();
            return;
        }
    }
    if let Some(right) = match key.as_str() {
        "ArrowLeft" => Some(false),
        "ArrowRight" => Some(true),
        _ => None,
    } {
        if let Some(did) = session::move_visual(session, right, shift) {
            if did != Did::Nothing {
                redraw(session);
                event.prevent_default();
                return;
            }
        }
    }
    let did = session.borrow_mut().edit(|editor| {
        match (ctrl, key.as_str()) {
            (_, "ArrowLeft") => editor.move_h(false, shift),
            (_, "ArrowRight") => editor.move_h(true, shift),
            (true, "ArrowUp") if !event.alt_key() && !shift => editor.annotate(true),
            (true, "ArrowDown") if !event.alt_key() && !shift => editor.annotate(false),
            (true, "ArrowUp") if shift => editor.move_document_edge(false, true),
            (true, "ArrowDown") if shift => editor.move_document_edge(true, true),
            (false, "ArrowUp") if event.alt_key() => editor.move_lines_vertical(false),
            (false, "ArrowDown") if event.alt_key() => editor.move_lines_vertical(true),
            (false, "ArrowUp") => editor.move_v(false, shift),
            (false, "ArrowDown") => editor.move_v(true, shift),
            // Alt+Up/Down は行を移動します。 Ctrl+Up/Down は上または下の共通欄を開きます。
            (true, "ArrowUp") | (true, "ArrowDown") => Did::Nothing,
            (false, "PageUp") => editor.move_page(false, shift),
            (false, "PageDown") => editor.move_page(true, shift),
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
            (true, "=") | (true, "+") => {
                crate::settings::zoom_in();
                Did::Nothing
            }
            (true, "-") | (true, "_") => {
                crate::settings::zoom_out();
                Did::Nothing
            }
            (true, "0") => {
                crate::settings::zoom_reset();
                Did::Nothing
            }
            (false, "z") if event.alt_key() => {
                crate::settings::toggle_whitespace();
                Did::Nothing
            }
            // 元に戻す、やり直し、およびクリップボードは、テキスト内でも構造内でも同様にウィンドウ ショートカットによって 1 回処理されます。
            (true, _) => Did::Nothing,
            // タブはテキスト内の列区切り文字であり、構造内の次のスロットです。
            (false, "Tab") => editor.tab(shift),
            // 印刷可能なキーは入力イベントとして到着し、IME もカバーします。
            (false, _) => Did::Nothing,
        }
    });
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
        let did = session.borrow_mut().edit(&edit);
        match did {
            Did::Changed => changed(&session),
            Did::Moved => redraw(&session),
            Did::Nothing => {}
        }
    }
    true
}
