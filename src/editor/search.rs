//! Search and replace over the model, with an optional regular expression.
//!
//! The browser's own regular expressions do the matching, so a bad pattern can
//! only fail to compile, never break the editor. Structures are searched as
//! well, row by row: a match is a stretch of characters that sit next to each
//! other, wherever in the document they are. A match never spans a structure's
//! edge, the same way it never spans a line's.

use js_sys::RegExp;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

use super::model::{Item, Pos, Sel, Text};
use crate::structure::ast::{Cursor, Node, Row};

/// Where a match is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    /// A stretch of ordinary text.
    Text(Sel),
    /// A stretch of one row inside the structure standing at `at`.
    Inside { at: Pos, cursor: Cursor },
}

/// A match: where it is, and the groups it captured.
#[derive(Clone, Debug)]
pub struct Found {
    pub place: Place,
    pub groups: Vec<String>,
}

/// A place in reading order, which is how "find next" carries on from the last
/// match: the item in the text, then how deep into the structure there it goes.
pub type Key = (Pos, Option<(Vec<(usize, usize)>, usize)>);

impl Place {
    pub fn start(&self) -> Key {
        match self {
            Place::Text(sel) => (sel.start(), None),
            Place::Inside { at, cursor } => (*at, Some((cursor.path.clone(), cursor.start()))),
        }
    }

    pub fn end(&self) -> Key {
        match self {
            Place::Text(sel) => (sel.end(), None),
            Place::Inside { at, cursor } => (*at, Some((cursor.path.clone(), cursor.end()))),
        }
    }
}

/// Where searching starts when nothing has been found yet: the caret, be it in
/// the text or inside a structure.
pub fn key_at(at: Pos, inside: Option<&Cursor>) -> Key {
    (at, inside.map(|cursor| (cursor.path.clone(), cursor.end())))
}

#[derive(Clone, Copy, Default)]
pub struct SearchOptions {
    pub regex: bool,
    pub case_sensitive: bool,
}

pub fn find_next(text: &Text, query: &str, options: SearchOptions, from: Key) -> Option<Found> {
    let all = find_all(text, query, options);
    all.iter()
        .find(|found| found.place.start() >= from)
        .or_else(|| all.first())
        .cloned()
}

pub fn find_all(text: &Text, query: &str, options: SearchOptions) -> Vec<Found> {
    let Some(regex) = compile(query, options) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for line in 0..text.line_count() {
        let items = text.line(line);
        for (start, run) in runs(items) {
            for (from, to, groups) in matches(&regex, &run) {
                found.push(Found {
                    place: Place::Text(Sel::range(
                        Pos::new(line, start + from),
                        Pos::new(line, start + to),
                    )),
                    groups,
                });
            }
        }
        // The rows inside a structure are searched too, so a name buried in a
        // fraction is found like any other.
        for (col, item) in items.iter().enumerate() {
            let Item::Math(root) = item else { continue };
            let at = Pos::new(line, col);
            search_row(&regex, root, &mut Vec::new(), &mut |cursor, groups| {
                found.push(Found {
                    place: Place::Inside { at, cursor },
                    groups,
                })
            });
        }
    }
    found.sort_by_key(|found| found.place.start());
    found
}

/// Searches one row and every row nested inside it, reporting each match as a
/// selection of that row.
fn search_row(
    regex: &RegExp,
    row: &Row,
    path: &mut Vec<(usize, usize)>,
    report: &mut impl FnMut(Cursor, Vec<String>),
) {
    for (start, run) in node_runs(row) {
        for (from, to, groups) in matches(regex, &run) {
            report(
                Cursor {
                    path: path.clone(),
                    anchor: start + from,
                    index: start + to,
                    fills: Vec::new(),
                },
                groups,
            );
        }
    }
    for (index, node) in row.iter().enumerate() {
        for slot in 0..node.slot_count() {
            let Some(nested) = node.slot(slot) else {
                continue;
            };
            path.push((index, slot));
            search_row(regex, nested, path, report);
            path.pop();
        }
    }
}

/// Every match in `run`, as the range of characters it covers and the groups it
/// captured.
fn matches(regex: &RegExp, run: &str) -> Vec<(usize, usize, Vec<String>)> {
    let mut found = Vec::new();
    regex.set_last_index(0);
    let units: Vec<usize> = char_starts(run);
    loop {
        let Some(result) = regex.exec(run) else {
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
        found.push((from, to, groups));
        if !regex.global() {
            break;
        }
    }
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

/// The same as `runs`, for a row of a structure: only plain characters can be
/// matched, so anything with a shape of its own breaks the run.
fn node_runs(row: &Row) -> Vec<(usize, String)> {
    let mut runs = Vec::new();
    let mut run = String::new();
    let mut start = 0;
    for (index, node) in row.iter().enumerate() {
        match node {
            Node::Char(c) => {
                if run.is_empty() {
                    start = index;
                }
                run.push(*c);
            }
            _ => {
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

    /// Only plain characters can be matched inside a structure, so a shape of
    /// its own breaks the run the same way a formula breaks a line.
    #[test]
    fn a_row_of_a_structure_is_searched_as_its_characters() {
        let row = vec![
            Node::Char('a'),
            Node::Char('b'),
            crate::structure::ast::sqrt(),
            Node::Char('c'),
        ];
        assert_eq!(
            node_runs(&row),
            vec![(0, "ab".to_string()), (3, "c".to_string())]
        );
    }

    /// Reading order: the text before a structure, then what is inside it, then
    /// the deeper rows, so finding the next match walks the document once.
    #[test]
    fn matches_are_ordered_by_where_they_are() {
        let text = Place::Text(Sel::range(Pos::new(0, 0), Pos::new(0, 1)));
        let shallow = Place::Inside {
            at: Pos::new(0, 1),
            cursor: Cursor::root(0),
        };
        let deep = Place::Inside {
            at: Pos::new(0, 1),
            cursor: Cursor {
                path: vec![(0, 0)],
                anchor: 0,
                index: 1,
                fills: Vec::new(),
            },
        };
        let mut places = vec![deep.clone(), text.clone(), shallow.clone()];
        places.sort_by_key(|place| place.start());
        assert_eq!(places, vec![text, shallow, deep]);
    }

    #[test]
    fn character_columns_come_from_utf16_offsets() {
        let starts = char_starts("あa𝑥b");
        assert_eq!(starts, vec![0, 1, 2, 4, 5]);
        assert_eq!(char_of(&starts, 2), Some(2));
    }
}
