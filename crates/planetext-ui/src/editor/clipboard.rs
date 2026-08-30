//! Clipboard fragments retain document nodes without passing through file notation.

use std::cell::RefCell;

use crate::structure::ast::{NodeKind, Row};
use crate::structure::plain;

#[derive(Clone)]
pub enum Clip {
    Text(Vec<Row>),
    Row(Row),
}

impl Clip {
    pub fn text(&self) -> String {
        match self {
            Clip::Text(lines) => plain::lines(lines),
            Clip::Row(row) => plain::row(row),
        }
    }

    pub fn lines(&self) -> Vec<Row> {
        match self {
            Clip::Text(lines) => lines.clone(),
            Clip::Row(row) => vec![row.clone()],
        }
    }

    /// Tabs have document-level behavior and are omitted when pasting into a nested slot.
    pub fn row(&self) -> Row {
        match self {
            Clip::Row(row) => row.clone(),
            Clip::Text(lines) => lines
                .iter()
                .flatten()
                .filter(|node| !matches!(node.kind, NodeKind::Tab))
                .cloned()
                .collect(),
        }
    }
}

thread_local! {
    static KEPT: RefCell<Option<(String, Clip)>> = const { RefCell::new(None) };
}

pub fn keep(clip: Clip) -> String {
    let text = clip.text();
    KEPT.with(|kept| *kept.borrow_mut() = Some((text.clone(), clip)));
    text
}

pub fn pasted(text: &str) -> Option<Clip> {
    KEPT.with(|kept| {
        kept.borrow()
            .as_ref()
            .filter(|(handed, _)| handed == text)
            .map(|(_, clip)| clip.clone())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::structure::ast::{Between, Node};

    fn fraction() -> Node {
        Node::stack(vec![Node::char('a')], vec![Node::char('b')], Between::Rule)
    }

    #[test]
    fn a_copied_structure_leaves_as_plain_text_and_comes_back_as_itself() {
        let text = keep(Clip::Text(vec![vec![fraction()]]));
        assert_eq!(text, "a/b");
        let clip = pasted(&text).expect("the piece that was copied");
        assert_eq!(clip.lines(), vec![vec![fraction()]]);
    }

    #[test]
    fn text_from_elsewhere_is_not_mistaken_for_the_copied_piece() {
        keep(Clip::Text(vec![vec![fraction()]]));
        assert!(pasted("a/b ").is_none());
        assert!(pasted("x/y").is_none());
    }

    #[test]
    fn a_row_of_characters_pastes_as_characters() {
        let clip = Clip::Row(vec![Node::char('a'), Node::char('b')]);
        assert_eq!(clip.text(), "ab");
        assert_eq!(clip.lines(), vec![vec![Node::char('a'), Node::char('b')]]);
    }

    #[test]
    fn nested_paste_drops_document_line_and_column_separators() {
        let clip = Clip::Text(vec![
            vec![Node::char('a'), Node::tab()],
            vec![Node::char('b')],
        ]);
        assert_eq!(clip.row(), vec![Node::char('a'), Node::char('b')]);
    }
}
