//! Search and replace over the model, with an optional regular expression.
//!
//! The browser's own regular expressions do the matching, so a bad pattern can
//! only fail to compile, never break the editor. Formulas are not searched: a
//! match can never start or end inside one.

use js_sys::RegExp;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

use super::model::{Item, Pos, Sel, Text};

#[derive(Clone, Copy, Default)]
pub struct SearchOptions {
    pub regex: bool,
    pub case_sensitive: bool,
}

/// A match: where it is, and the groups it captured.
pub type Match = (Sel, Vec<String>);

pub fn find_next(text: &Text, query: &str, options: SearchOptions, from: Pos) -> Option<Sel> {
    let all = find_all(text, query, options);
    all.iter()
        .find(|(sel, _)| sel.start() >= from)
        .or_else(|| all.first())
        .map(|(sel, _)| *sel)
}

pub fn find_all(text: &Text, query: &str, options: SearchOptions) -> Vec<Match> {
    let Some(regex) = compile(query, options) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for line in 0..text.line_count() {
        for (start, run) in runs(text.line(line)) {
            regex.set_last_index(0);
            let units: Vec<usize> = char_starts(&run);
            loop {
                let Some(result) = regex.exec(&run) else {
                    break;
                };
                let Some(whole) = result.get(0).as_string() else {
                    break;
                };
                let Some(at) = match_index(&result) else {
                    break;
                };
                let end = at + whole.encode_utf16().count();
                if whole.is_empty() {
                    // An empty match would loop for ever.
                    regex.set_last_index(regex.last_index() + 1);
                    if regex.last_index() as usize > run.encode_utf16().count() {
                        break;
                    }
                    continue;
                }
                let (Some(from), Some(to)) = (char_of(&units, at), char_of(&units, end)) else {
                    break;
                };
                let groups = (0..result.length())
                    .map(|i| result.get(i).as_string().unwrap_or_default())
                    .collect();
                found.push((
                    Sel::range(Pos::new(line, start + from), Pos::new(line, start + to)),
                    groups,
                ));
                if !regex.global() {
                    break;
                }
            }
        }
    }
    found.sort_by_key(|(sel, _)| sel.start());
    found
}

/// Fills `$1`-style references in the replacement with what was captured.
pub fn expand(groups: &[String], replacement: &str, options: SearchOptions) -> String {
    if !options.regex {
        return replacement.to_string();
    }
    let mut out = String::new();
    let mut chars = replacement.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('$') => {
                chars.next();
                out.push('$');
            }
            Some('&') => {
                chars.next();
                out.push_str(groups.first().map(String::as_str).unwrap_or_default());
            }
            Some(digit) if digit.is_ascii_digit() => {
                let mut number = String::new();
                while let Some(digit) = chars.peek().filter(|c| c.is_ascii_digit()) {
                    number.push(*digit);
                    chars.next();
                }
                let index: usize = number.parse().unwrap_or(0);
                out.push_str(groups.get(index).map(String::as_str).unwrap_or_default());
            }
            _ => out.push('$'),
        }
    }
    out
}

/// The stretches of ordinary characters in a line, with the column each starts
/// at. Formulas split them, so no match can span one.
fn runs(items: &[Item]) -> Vec<(usize, String)> {
    let mut runs = Vec::new();
    let mut run = String::new();
    let mut start = 0;
    for (col, item) in items.iter().enumerate() {
        match item.as_char() {
            Some(c) => {
                if run.is_empty() {
                    start = col;
                }
                run.push(c);
            }
            None => {
                if !run.is_empty() {
                    runs.push((start, std::mem::take(&mut run)));
                }
            }
        }
    }
    if !run.is_empty() {
        runs.push((start, run));
    }
    runs
}

/// The UTF-16 index each character starts at, since that is what the browser's
/// regular expressions count in.
fn char_starts(text: &str) -> Vec<usize> {
    let mut units = Vec::with_capacity(text.chars().count() + 1);
    let mut at = 0;
    for c in text.chars() {
        units.push(at);
        at += c.len_utf16();
    }
    units.push(at);
    units
}

/// Where a match starts, in UTF-16 units, as the browser reports it.
fn match_index(result: &js_sys::Array<js_sys::JsString>) -> Option<usize> {
    js_sys::Reflect::get(result, &JsValue::from_str("index"))
        .ok()?
        .as_f64()
        .map(|index| index as usize)
}

fn char_of(starts: &[usize], unit: usize) -> Option<usize> {
    starts.iter().position(|start| *start == unit)
}

/// Compiles the query, treating a plain query as literal text. A pattern the
/// browser rejects gives `None` instead of an error.
fn compile(query: &str, options: SearchOptions) -> Option<RegExp> {
    if query.is_empty() {
        return None;
    }
    let source = if options.regex {
        query.to_string()
    } else {
        escape_regex(query)
    };
    let flags = if options.case_sensitive { "g" } else { "gi" };
    let make = js_sys::Function::new_with_args(
        "source, flags",
        "try { return new RegExp(source, flags); } catch (e) { return null; }",
    );
    make.call2(&JsValue::NULL, &source.into(), &flags.into())
        .ok()?
        .dyn_into::<RegExp>()
        .ok()
}

fn escape_regex(query: &str) -> String {
    let mut out = String::with_capacity(query.len());
    for c in query.chars() {
        if "\\^$.|?*+()[]{}".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_queries_are_escaped_into_literals() {
        assert_eq!(escape_regex("a.b*"), "a\\.b\\*");
    }

    #[test]
    fn groups_fill_the_replacement() {
        let groups = vec!["a@b".to_string(), "a".to_string(), "b".to_string()];
        let options = SearchOptions {
            regex: true,
            case_sensitive: false,
        };
        assert_eq!(expand(&groups, "$2:$1", options), "b:a");
        assert_eq!(expand(&groups, "[$&]", options), "[a@b]");
        assert_eq!(expand(&groups, "$$1", options), "$1");
        // Without the regular expression box the replacement is literal.
        assert_eq!(expand(&groups, "$1", SearchOptions::default()), "$1");
    }

    #[test]
    fn islands_break_up_the_searched_text() {
        let text = Text::from_lines(vec![vec![
            Item::Char('a'),
            Item::Char('b'),
            Item::Math(vec![crate::structure::ast::Node::Char('x')]),
            Item::Char('c'),
            Item::Char('d'),
        ]]);
        let runs = runs(text.line(0));
        assert_eq!(runs, vec![(0, "ab".to_string()), (3, "cd".to_string())]);
    }

    #[test]
    fn character_columns_come_from_utf16_offsets() {
        let starts = char_starts("あa𝑥b");
        assert_eq!(starts, vec![0, 1, 2, 4, 5]);
        assert_eq!(char_of(&starts, 2), Some(2));
    }
}
