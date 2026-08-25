//! テキストから直接構造を開始するショートカットの仕組み: `1/ `、`x^ `、`\sqrt `は構造に切り替わりますが、`\alpha ` と `\sin ` は通常の文字のみを入れます。

use std::cell::RefCell;
use std::rc::Rc;

use super::session::{self, Session};
use crate::structure::ast::{self, Node as StructureNode};
use crate::structure::text::{as_char, Pos};
use crate::structure::vocabulary;

enum Seed {
    /// `1/`、`x^`: すでに入力されているテキスト、その後、 trigger.
    Typed(String, char),
    /// `\sqrt `: コマンド名の構造体。
    Node(StructureNode),
    NthRoot(String),
    /// `\alpha `、`\sin `: 通常のテキスト。
    Text(String),
}

/// 構造ショートカットを完了する入力文字を処理し、入力を消費したかを返します。
pub fn type_char(session: &Rc<RefCell<Session>>, c: char) -> bool {
    let (sel, is_nested, sels) = {
        let borrowed = session.borrow();
        let editor = borrowed.editor.borrow();
        (
            editor.primary(),
            editor.nested_cursor().is_some(),
            editor.sels(),
        )
    };
    if !sel.is_caret() || is_nested {
        return false;
    }
    if sels.len() > 1 {
        let all_trigger = sels.iter().all(|sel| {
            sel.is_caret()
                && seed_for(c, &text_before(session, sel.head))
                    .is_some_and(|(_, seed)| !matches!(seed, Seed::Text(_)))
        });
        if !all_trigger {
            return false;
        }
        {
            let borrowed = session.borrow();
            let mut editor = borrowed.editor.borrow_mut();
            editor.start_structure();
            editor.insert_text(&c.to_string());
        }
        session::changed(session);
        return true;
    }
    let before = text_before(session, sel.head);
    let Some((consume, seed)) = seed_for(c, &before) else {
        return false;
    };
    let from = Pos::new(sel.head.line, sel.head.col - consume);
    match seed {
        Seed::Text(text) => {
            session
                .borrow()
                .editor
                .borrow_mut()
                .replace_range(from, sel.head, &text);
            session::changed(session);
            true
        }
        seed => {
            {
                let borrowed = session.borrow();
                let mut editor = borrowed.editor.borrow_mut();
                // ショートカット文字列の置換と構造の配置を、履歴上の1操作にまとめます。
                editor.one_step(|editor| {
                    editor.replace_range(from, sel.head, "");
                    editor.start_structure();
                    // 本文トリガーの編集状態は、今回作る構造が完成した時点で終了します。
                    editor.limit_trigger_to_structure();
                    match seed {
                        Seed::Text(_) => {}
                        Seed::Typed(run, trigger) => {
                            // 一度に 1 文字ずつ、同じドアタイピングが使用するため、`1/` は手動で構築する構造を構築します。
                            let mut buffer = [0u8; 4];
                            for c in run.chars().chain(std::iter::once(trigger)) {
                                editor.insert_text(c.encode_utf8(&mut buffer));
                            }
                        }
                        Seed::Node(node) => {
                            editor.insert_node(node);
                        }
                        Seed::NthRoot(index) => {
                            editor.insert_node(ast::nth_root());
                            editor.insert_text(&index);
                            editor.tab(false);
                        }
                    }
                });
            }
            session::focus();
            session::changed(session);
            true
        }
    }
}

/// キャレットの前にある、構造Nodeを空白として読み替えた文字列。
fn text_before(session: &Rc<RefCell<Session>>, at: Pos) -> String {
    let editor_rc = session.borrow().editor.clone();
    let editor = editor_rc.borrow();
    editor
        .text()
        .line(at.line)
        .iter()
        .take(at.col)
        .map(|node| as_char(node).unwrap_or(' '))
        .collect()
}

/// トリガー文字が生成するもの、およびそれに伴うキャレットの前の文字数。
fn seed_for(c: char, before: &str) -> Option<(usize, Seed)> {
    (c == ' ')
        .then(|| trailing_typed(before).or_else(|| trailing_shortcut(before)))
        .flatten()
}

fn trailing_typed(text: &str) -> Option<(usize, Seed)> {
    let trigger = text
        .chars()
        .next_back()
        .filter(|c| matches!(c, '/' | '^' | '_'))?;
    let before = text.strip_suffix(trigger)?;
    let (consumed, run) = match (trigger == '/').then(|| trailing_group(before)).flatten() {
        Some(group) => group,
        None => {
            let run = trailing_run(before);
            (run.chars().count(), run)
        }
    };
    (!run.is_empty()).then(|| (consumed + 1, Seed::Typed(run, trigger)))
}

fn trailing_group(text: &str) -> Option<(usize, String)> {
    if !text.ends_with(')') {
        return None;
    }
    let mut depth = 0;
    for (start, c) in text.char_indices().rev() {
        match c {
            ')' => depth += 1,
            '(' if depth == 1 => {
                let run = &text[start + 1..text.len() - 1];
                return (!run.is_empty()).then(|| (text[start..].chars().count(), run.to_string()));
            }
            '(' => depth -= 1,
            _ => {}
        }
    }
    None
}

fn trailing_run(text: &str) -> String {
    let run: String = text
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '.')
        .collect();
    run.chars().rev().collect()
}

/// `\name` または `√` などの直接入力されたグリフ。文字を表すだけの名前はテキストのままです。
fn trailing_shortcut(text: &str) -> Option<(usize, Seed)> {
    if let Some(root) = trailing_root(text) {
        return Some(root);
    }
    if let Some(name) = trailing_command(text) {
        let consumed = name.chars().count() + 1;
        if let Some(node) = vocabulary::structure_for(&name) {
            return Some((consumed, Seed::Node(node)));
        }
        if let Some(replacement) = vocabulary::text_for(&name) {
            return Some((consumed, Seed::Text(replacement)));
        }
    }
    let glyph = text.chars().next_back()?;
    let node = vocabulary::node_for_glyph(glyph)?;
    Some((1, Seed::Node(node)))
}

fn trailing_root(text: &str) -> Option<(usize, Seed)> {
    let text = text.strip_suffix(']')?;
    let (before, index) = text.rsplit_once("√[")?;
    (!index.is_empty() && index.chars().all(char::is_alphanumeric)).then(|| {
        (
            text[before.len()..].chars().count() + 1,
            Seed::NthRoot(index.to_string()),
        )
    })
}

fn trailing_command(text: &str) -> Option<String> {
    let letters: String = text
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect();
    if letters.is_empty() {
        return None;
    }
    let name: String = letters.chars().rev().collect();
    let start = text.len() - name.len();
    text[..start].ends_with('\\').then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(c: char, before: &str) -> Option<(usize, String)> {
        seed_for(c, before).map(|(consume, seed)| {
            let kind = match seed {
                Seed::Typed(run, trigger) => format!("typed {run}{trigger}"),
                Seed::Node(_) => "node".to_string(),
                Seed::NthRoot(index) => format!("root {index}"),
                Seed::Text(text) => format!("text {text}"),
            };
            (consume, kind)
        })
    }

    #[test]
    fn slash_waits_for_space_after_every_kind_of_text() {
        assert!(seed('/', "abc").is_none());
        assert_eq!(seed(' ', "abc/"), Some((4, "typed abc/".into())));
        assert_eq!(seed(' ', "日本/"), Some((3, "typed 日本/".into())));
        assert_eq!(seed(' ', "(x+1)/"), Some((6, "typed x+1/".into())));
    }

    #[test]
    fn commands_only_become_formulas_when_they_need_a_shape() {
        assert_eq!(seed(' ', "\\sqrt"), Some((5, "node".into())));
        assert_eq!(seed(' ', "\\alpha"), Some((6, "text α".into())));
        assert_eq!(seed(' ', "\\sin"), Some((4, "text sin".into())));
        assert!(seed(' ', "hello").is_none());
    }

    #[test]
    fn structural_glyphs_wait_for_space_but_plain_symbols_do_not_expand() {
        assert!(seed('√', "").is_none());
        assert_eq!(seed(' ', "√"), Some((1, "node".into())));
        assert_eq!(seed(' ', "√[n]"), Some((4, "root n".into())));
        assert!(seed(' ', "√[]").is_none());
        assert_eq!(seed(' ', "Σ"), Some((1, "node".into())));
        assert!(seed(' ', "α").is_none());
    }
}
