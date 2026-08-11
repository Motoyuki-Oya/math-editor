//! Starting a formula straight from the text, the way Markdown shortcuts work:
//! `1/`, `x^`, `\sqrt ` and `$` switch into a formula, while `\alpha ` and
//! `\sin ` only put ordinary characters in the text.

use std::cell::RefCell;
use std::rc::Rc;

use super::model::Pos;
use super::state::{self, Session};
use crate::structure::ast::Node as MathNode;
use crate::structure::commands;

enum Seed {
    /// `$`: an empty formula.
    Empty,
    /// `1/`, `x^`: text that was already typed, then the trigger.
    Typed(String, char),
    /// `\sqrt `: the structure the command names.
    Node(MathNode),
    /// `\alpha `, `\sin `: ordinary text, no formula needed.
    Text(String),
}

/// Handles a typed character that may start a formula. Returns whether it did,
/// in which case the caller must not insert the character itself.
pub fn type_char(session: &Rc<RefCell<Session>>, c: char) -> bool {
    let sel = session.borrow().editor.primary();
    if !sel.is_caret() || session.borrow().editor.sels().len() > 1 {
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
            state::changed(session);
            true
        }
        seed => {
            {
                let mut borrowed = session.borrow_mut();
                borrowed.editor.replace_range(from, sel.head, "");
            }
            state::insert_math();
            let Some(session) = state::session() else {
                return true;
            };
            match seed {
                Seed::Empty | Seed::Text(_) => {}
                Seed::Typed(run, trigger) => {
                    if let Some(active) = session.borrow_mut().active.as_mut() {
                        for c in run.chars() {
                            active.state.insert_char(c);
                        }
                        active.state.insert_char(trigger);
                    }
                }
                Seed::Node(node) => {
                    if let Some(active) = session.borrow_mut().active.as_mut() {
                        active.state.insert(node);
                    }
                }
            }
            state::write_back(&session);
            state::redraw(&session);
            true
        }
    }
}

/// The characters on the line before the caret, formulas left out.
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

/// What a trigger character produces, and how many characters before the caret
/// it takes with it.
fn seed_for(c: char, before: &str) -> Option<(usize, Seed)> {
    match c {
        '$' => Some((0, Seed::Empty)),
        '/' | '^' | '_' => {
            let run = trailing_run(before);
            // `and/or` should stay prose; `1/`, `x/` and `x^` are formulas.
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

/// A `\name` or a directly typed glyph such as `√`. Names that only stand for a
/// character stay text.
fn trailing_shortcut(text: &str) -> Option<(usize, Seed)> {
    if let Some(name) = trailing_command(text) {
        if let Some(node) = commands::node_for(&name) {
            let consumed = name.chars().count() + 1;
            let seed = match node {
                MathNode::Sym(name) => Seed::Text(commands::glyph_for(&name)?.to_string()),
                MathNode::Func(name) => Seed::Text(name),
                node => Seed::Node(node),
            };
            return Some((consumed, seed));
        }
    }
    let glyph = text.chars().next_back()?;
    let node = commands::node_for_glyph(glyph)?;
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
