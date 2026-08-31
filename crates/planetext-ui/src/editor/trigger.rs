//! どの深さでも同じ「文字列 + トリガー文字 + スペース」で構造へ移る。

use std::cell::RefCell;
use std::rc::Rc;

use super::session::{self, Session};

/// 構造ショートカットを完了する入力文字を処理し、入力を消費したかを返します。
pub fn type_char(session: &Rc<RefCell<Session>>, c: char) -> bool {
    let converted = session.borrow_mut().edit(|editor| editor.convert_typed(c));
    if converted {
        if session.borrow().nested_cursor().is_some() {
            session::focus();
        }
        session::changed(session);
    }
    converted
}
