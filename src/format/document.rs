//! Reading a whole document from a file and writing it back.
//!
//! This is the only place that knows a document ever becomes text.

use super::islands::{self, Segment};
use super::notation::{island_text, parse_island};
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

/// The lines of a copied range, for the clipboard. Every island is written out
/// so that pasting the text anywhere gives back the same structures.
pub fn write_items(lines: &[Vec<Item>]) -> String {
    let lines: Vec<islands::Line> = lines.iter().map(line_segments).collect();
    islands::serialize(&lines)
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
    use crate::structure::ast::Node;
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
}
