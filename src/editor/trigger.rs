//! テキストから直接数式を開始する、Markdown ショートカットの仕組み: `1/`、`x^`、`\sqrt `、および `$` は数式に切り替わりますが、`\alpha ` と `\sin ` はテキストに通常の文字のみを入れます。

use std::cell::RefCell;
use std::rc::Rc;

use super::session::{self, Session};
use crate::structure::ast::Node as MathNode;
use crate::structure::text::Pos;
use crate::structure::vocabulary;

enum Seed {
    /// `$`: 空の数式。
    Empty,
    /// `1/`、`x^`: すでに入力されているテキスト、その後、 trigger.
    Typed(String, char),
    /// `\sqrt `: コマンド名の構造体。
    Node(MathNode),
    /// `\alpha `、`\sin `: 通常のテキスト。数式は必要ありません。
    Text(String),
}

/// 数式を開始する可能性がある入力文字を処理します。挿入したかどうかを返します。挿入した場合、呼び出し元は文字自体を挿入してはなりません。
pub fn type_char(session: &Rc<RefCell<Session>>, c: char) -> bool {
    let sel = session.borrow().editor.primary();
    if !sel.is_caret() || session.borrow().editor.sels().len() > 1 {
        return false;
    }
    // 式の内部には、構造体自体のショートカットがあります。これらは、通常のテキストを数式に変換するだけです。
    if session.borrow().editor.inside().is_some() {
        return false;
    }
    let before = text_before(session, sel.head);
    let Some((consume, seed)) = seed_for(c, &before) else {
        return false;
    };
    let from = Pos::new(sel.head.line, sel.head.col - consume);
    match seed {
        Seed::Text(text) => {
            session
                .borrow_mut()
                .editor
                .replace_range(from, sel.head, &text);
            session::changed(session);
            true
        }
        seed => {
            {
                let mut borrowed = session.borrow_mut();
                // 入力されたテキストを取得してその構造を作成することは、歴史の 1 ステップです。`1/` を元に戻すと、入力した空の数式ではなく、文字が戻ります。
                borrowed.editor.one_step(|editor| {
                    editor.replace_range(from, sel.head, "");
                    editor.insert_island();
                    // 誰も求めなかった数式は、それを呼び出した構造が続く限り持続します。つまり、`1/2 + 3` は分数を数式に入れ、`+ 3` をテキストに戻します。
                    if !matches!(seed, Seed::Empty) {
                        editor.island_lasts_one_structure();
                    }
                    match seed {
                        Seed::Empty | Seed::Text(_) => {}
                        Seed::Typed(run, trigger) => {
                            // 一度に 1 文字ずつ、同じドアタイピングが使用するため、`1/` は手動で構築する構造を構築します。
                            let mut buffer = [0u8; 4];
                            for c in run.chars().chain(std::iter::once(trigger)) {
                                editor.insert_text(c.encode_utf8(&mut buffer));
                            }
                        }
                        Seed::Node(node) => {
                            editor.insert_in_island(node);
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

/// キャレットの前の行の文字、省略された数式。
fn text_before(session: &Rc<RefCell<Session>>, at: Pos) -> String {
    let borrowed = session.borrow();
    borrowed
        .editor
        .text()
        .line(at.line)
        .iter()
        .take(at.col)
        .map(|item| item.as_char().unwrap_or(' '))
        .collect()
}

/// トリガー文字が生成するもの、およびそれに伴うキャレットの前の文字数。
fn seed_for(c: char, before: &str) -> Option<(usize, Seed)> {
    match c {
        '$' => Some((0, Seed::Empty)),
        '/' | '^' | '_' => {
            let run = trailing_run(before);
            // `and/or` は散文のままである必要があります。 `1/`、`x/`、および `x^` は数式です。
            let mathlike = !run.is_empty()
                && (c != '/'
                    || run.chars().any(|c| c.is_ascii_digit())
                    || run.chars().count() == 1);
            mathlike.then(|| (run.chars().count(), Seed::Typed(run, c)))
        }
        ' ' => trailing_shortcut(before),
        _ => None,
    }
}

fn trailing_run(text: &str) -> String {
    let run: String = text
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '.')
        .collect();
    run.chars().rev().collect()
}

/// `\name` または `√` などの直接入力されたグリフ。文字を表すだけの名前はテキストのままです。
fn trailing_shortcut(text: &str) -> Option<(usize, Seed)> {
    if let Some(name) = trailing_command(text) {
        if let Some(node) = vocabulary::node_for(&name) {
            let consumed = name.chars().count() + 1;
            let seed = match node {
                MathNode::Sym(name) => Seed::Text(vocabulary::glyph_for(&name)?.to_string()),
                MathNode::Func(name) => Seed::Text(name),
                node => Seed::Node(node),
            };
            return Some((consumed, seed));
        }
    }
    let glyph = text.chars().next_back()?;
    let node = vocabulary::node_for_glyph(glyph)?;
    Some((1, Seed::Node(node)))
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
                Seed::Empty => "empty".to_string(),
                Seed::Typed(run, trigger) => format!("typed {run}{trigger}"),
                Seed::Node(_) => "node".to_string(),
                Seed::Text(text) => format!("text {text}"),
            };
            (consume, kind)
        })
    }

    #[test]
    fn slash_after_a_word_stays_prose() {
        assert!(seed('/', "and").is_none());
        assert_eq!(seed('/', "1"), Some((1, "typed 1/".into())));
        assert_eq!(seed('/', "x"), Some((1, "typed x/".into())));
    }

    #[test]
    fn commands_only_become_formulas_when_they_need_a_shape() {
        assert_eq!(seed(' ', "\\sqrt"), Some((5, "node".into())));
        assert_eq!(seed(' ', "\\alpha"), Some((6, "text α".into())));
        assert_eq!(seed(' ', "\\sin"), Some((4, "text sin".into())));
        assert!(seed(' ', "hello").is_none());
    }

    #[test]
    fn a_root_glyph_expands_but_a_greek_letter_does_not() {
        assert_eq!(seed(' ', "√"), Some((1, "node".into())));
        assert!(seed(' ', "α").is_none());
    }

    #[test]
    fn dollar_opens_an_empty_formula() {
        assert_eq!(seed('$', "text "), Some((0, "empty".into())));
    }
}
