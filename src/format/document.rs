//! Reads and writes a complete document. `$()` belongs only to this file-format layer.

use super::islands::{self, Segment};
use super::notation::{island_text, parse_island};
use crate::structure::ast::{Node, NodeKind, Row};
use crate::structure::text::Text;

pub const TAB_SOURCE: &str = "t";

pub fn read(source: &str) -> Text {
    let lines = islands::parse(source)
        .into_iter()
        .map(|line| {
            line.into_iter()
                .flat_map(|segment| match segment {
                    Segment::Text(text) => text.chars().map(Node::char).collect::<Row>(),
                    Segment::Island(source) if source.trim() == TAB_SOURCE => vec![Node::tab()],
                    Segment::Island(source) => parse_island(&source),
                })
                .collect()
        })
        .collect();
    Text::from_lines(lines)
}

pub fn write(text: &Text) -> String {
    let lines: Vec<islands::Line> = text.lines().iter().map(line_segments).collect();
    islands::serialize(&lines)
}

fn line_segments(row: &Row) -> islands::Line {
    let mut segments = islands::Line::new();
    let mut text = String::new();
    for node in row {
        let source = match &node.kind {
            NodeKind::Char(c) if node.upper.is_empty() && node.lower.is_empty() => {
                text.push(*c);
                continue;
            }
            NodeKind::Tab if node.upper.is_empty() && node.lower.is_empty() => {
                TAB_SOURCE.to_string()
            }
            _ => island_text(&vec![node.clone()]),
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
    use crate::structure::ast::NodeKind;
    use crate::structure::text::Pos;

    #[test]
    fn structures_are_sibling_nodes_in_the_document_row() {
        let text = read("a + $(b/c) + d");
        assert_eq!(text.line_len(0), 9);
        assert!(matches!(
            text.node_at(Pos::new(0, 4)).map(|n| &n.kind),
            Some(NodeKind::Stack { .. })
        ));
        assert!(matches!(
            text.node_at(Pos::new(0, 5)).map(|n| &n.kind),
            Some(NodeKind::Char(' '))
        ));
    }

    #[test]
    fn a_column_separator_is_one_node() {
        let text = read("a $(t) b");
        assert_eq!(text.line_len(0), 5);
        assert!(matches!(
            text.node_at(Pos::new(0, 2)).map(|n| &n.kind),
            Some(NodeKind::Tab)
        ));
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
    fn a_document_with_many_structures_survives_a_load_and_save() {
        let source = "x = $(1/2) + $(√ y$(^ 2))\n$({[x>0, 正][x<0, 負])\na $(t) b $(t) c\n$(↨ Σ, n, i=1)i";
        let text = read(source);
        assert_eq!(write(&text), source);
        assert_eq!(read(&write(&text)), text);
    }

    #[test]
    fn grouped_plain_characters_may_save_canonically() {
        let text = read("a + $(b/c) + d");
        assert_eq!(write(&text), "a + $(b/c) + d");
        assert_eq!(read(&write(&text)), text);
    }
}
