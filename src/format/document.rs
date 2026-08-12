//! Reading a whole document from a file and writing it back.
//!
//! This is the only place that knows a document ever becomes text.

use super::islands::{self, Segment};
use super::notation::{island_text, parse_island};
use crate::structure::ast::{Node, Row};
use crate::structure::text::{Item, Text};

/// The island that means a column separator rather than a structure.
pub const TAB_SOURCE: &str = "t";

pub fn read(source: &str) -> Text {
    let lines = islands::parse(source)
        .into_iter()
        .map(|line| {
            line.into_iter()
                .flat_map(|segment| match segment {
                    Segment::Text(text) => text.chars().map(Item::Char).collect::<Vec<_>>(),
                    Segment::Island(source) if source.trim() == TAB_SOURCE => vec![Item::Tab],
                    Segment::Island(source) => vec![Item::Math(parse_island(&source))],
                })
                .collect()
        })
        .collect::<Vec<Vec<Item>>>();
    Text::from_lines(lines)
}

pub fn write(text: &Text) -> String {
    let lines: Vec<islands::Line> = text.lines().iter().map(line_segments).collect();
    islands::serialize(&lines)
}

/// The lines of a copied range, for the clipboard.
///
/// Islands are written out, so a structure can be pasted somewhere else, but
/// the characters around them are left as they are: pasting does not read the
/// notation, so escaping a `$` here would paste it back doubled.
pub fn write_items(lines: &[Vec<Item>]) -> String {
    lines
        .iter()
        .map(|items| {
            items
                .iter()
                .map(|item| match item {
                    Item::Char(c) => c.to_string(),
                    Item::Tab => format!("$({TAB_SOURCE})"),
                    Item::Math(row) => format!("$({})", island_text(row)),
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A piece copied out of a structure. Plain characters come out as themselves,
/// so taking a word out of a formula gives that word; anything with a shape
/// comes out as a structure, which can be pasted back as one.
pub fn write_row(row: &Row) -> String {
    let plain: Option<String> = row
        .iter()
        .map(|node| match node {
            Node::Char(c) => Some(*c),
            _ => None,
        })
        .collect();
    match plain {
        Some(text) => text,
        None => format!("$({})", island_text(row)),
    }
}

/// Reads copied text as the pieces of a structure, so anything copied out of
/// the document can be pasted inside one. Characters stay characters: pasting
/// never re-runs the shortcuts that typing them would.
pub fn read_row(text: &str) -> Row {
    islands::parse(text)
        .into_iter()
        .flatten()
        .flat_map(|segment| match segment {
            Segment::Text(text) => text.chars().map(Node::Char).collect::<Row>(),
            Segment::Island(source) if source.trim() == TAB_SOURCE => Row::new(),
            Segment::Island(source) => parse_island(&source),
        })
        .collect()
}

fn line_segments(items: &Vec<Item>) -> islands::Line {
    let mut segments = islands::Line::new();
    let mut text = String::new();
    for item in items {
        let source = match item {
            Item::Char(c) => {
                text.push(*c);
                continue;
            }
            Item::Tab => TAB_SOURCE.to_string(),
            Item::Math(row) => island_text(row),
        };
        if !text.is_empty() {
            segments.push(Segment::Text(std::mem::take(&mut text)));
        }
        segments.push(Segment::Island(source));
    }
    if !text.is_empty() {
        segments.push(Segment::Text(text));
    }
    segments
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::structure::text::Pos;

    #[test]
    fn an_island_is_one_item_holding_the_structure() {
        let text = read("a $(1/2) b");
        assert_eq!(text.line_len(0), 5);
        let Some(Item::Math(row)) = text.item_at(Pos::new(0, 2)) else {
            panic!("an island");
        };
        assert!(matches!(row.as_slice(), [Node::Stack { .. }]));
    }

    #[test]
    fn a_column_separator_is_one_item() {
        let text = read("a $(t) b");
        assert_eq!(text.line_len(0), 5);
        assert_eq!(text.item_at(Pos::new(0, 2)), Some(&Item::Tab));
    }

    #[test]
    fn documents_survive_a_load_and_save() {
        for source in [
            "a $(1/2) b",
            "a $(t) b",
            "$(√[3] x)",
            "x$(^ 3)$(_ i)",
            "$(↨ Σ, n, x=1)",
            "$([a, b][c, d])",
            "$(a → b)",
            "$(√ x$(^ 3))",
            "100$$ です",
        ] {
            assert_eq!(write(&read(source)), source);
        }
    }

    #[test]
    fn copied_islands_can_be_pasted_back() {
        let text = read("$(1/2)x");
        let copied = write_items(text.lines());
        assert_eq!(copied, "$(1/2)x");
        assert_eq!(read(&copied), text);
    }

    /// Pasting inserts the characters as they are, so copying must not escape
    /// them: a copied `$` has to stay one `$`.
    #[test]
    fn a_copied_dollar_sign_stays_one_character() {
        assert_eq!(write_items(read("100$$ です").lines()), "100$ です");
    }

    #[test]
    fn a_piece_of_a_structure_without_a_shape_is_copied_as_plain_text() {
        let text = read("$(ab)");
        let Some(Item::Math(row)) = text.item_at(Pos::new(0, 0)) else {
            panic!("an island");
        };
        assert_eq!(write_row(row), "ab");
    }

    #[test]
    fn a_piece_of_a_structure_survives_a_copy_and_paste() {
        for source in ["$(1/2)", "$(√ x)", "$(a/b)$(c/d)"] {
            let row = read_row(source);
            assert_ne!(row, Row::new());
            assert_eq!(read_row(&write_row(&row)), row);
        }
    }

    /// Text pasted inside a structure keeps its characters: a `/` does not turn
    /// into a fraction the way typing one would.
    #[test]
    fn pasted_characters_stay_characters() {
        let expected: Row = "a/b".chars().map(Node::Char).collect();
        assert_eq!(read_row("a/b"), expected);
    }
}
