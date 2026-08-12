//! What copying puts on the clipboard, and what pasting makes of what is there.
//!
//! The clipboard is not a file, so nothing here goes through the file format:
//! what leaves the editor is ordinary text, read the way the document reads on
//! screen. The piece itself is kept here beside the text that went out, so a
//! copy that comes back into the editor keeps its shape, while text from
//! anywhere else arrives as the plain characters it is.

use std::cell::RefCell;

use crate::structure::ast::{Node, Row};
use crate::structure::plain;
use crate::structure::text::Item;

/// A piece of a document on its way through the clipboard.
#[derive(Clone)]
pub enum Clip {
    /// Lines of the text, structures included.
    Text(Vec<Vec<Item>>),
    /// Part of one row of a structure.
    Row(Row),
}

impl Clip {
    /// How the piece reads as ordinary text.
    pub fn text(&self) -> String {
        match self {
            Clip::Text(lines) => plain::lines(lines),
            Clip::Row(row) => plain::row(row),
        }
    }

    /// The piece as lines of the text: a row of plain characters is those
    /// characters, and a row with a shape becomes one structure.
    pub fn items(&self) -> Vec<Vec<Item>> {
        match self {
            Clip::Text(lines) => lines.clone(),
            Clip::Row(row) => {
                let chars: Option<Vec<Item>> = row
                    .iter()
                    .map(|node| match node {
                        Node::Char(c) => Some(Item::Char(*c)),
                        _ => None,
                    })
                    .collect();
                match chars {
                    Some(items) => vec![items],
                    None => vec![vec![Item::Math(row.clone())]],
                }
            }
        }
    }

    /// The piece as part of a row, for pasting inside a structure.
    pub fn row(&self) -> Row {
        match self {
            Clip::Row(row) => row.clone(),
            Clip::Text(lines) => lines
                .iter()
                .flatten()
                .flat_map(|item| match item {
                    Item::Char(c) => vec![Node::Char(*c)],
                    // A structure inside a structure is just its own things in
                    // the row; a column separator has no meaning in one.
                    Item::Math(row) => row.clone(),
                    Item::Tab => Vec::new(),
                })
                .collect(),
        }
    }
}

thread_local! {
    /// The last piece copied, with the text that was handed out for it.
    static KEPT: RefCell<Option<(String, Clip)>> = const { RefCell::new(None) };
}

/// Keeps a copied piece and returns the text to put on the clipboard.
pub fn keep(clip: Clip) -> String {
    let text = clip.text();
    KEPT.with(|kept| *kept.borrow_mut() = Some((text.clone(), clip)));
    text
}

/// The piece that was copied, when the pasted text is the one it went out as.
/// Anything else came from somewhere outside and is plain text.
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
    use crate::structure::ast::Between;

    fn fraction() -> Row {
        vec![Node::Stack {
            above: vec![Node::Char('a')],
            below: vec![Node::Char('b')],
            between: Between::Rule,
        }]
    }

    #[test]
    fn a_copied_structure_leaves_as_plain_text_and_comes_back_as_itself() {
        let text = keep(Clip::Text(vec![vec![Item::Math(fraction())]]));
        assert_eq!(text, "a/b");
        let clip = pasted(&text).expect("the piece that was copied");
        assert_eq!(clip.items(), vec![vec![Item::Math(fraction())]]);
    }

    #[test]
    fn text_from_elsewhere_is_not_mistaken_for_the_copied_piece() {
        keep(Clip::Text(vec![vec![Item::Math(fraction())]]));
        assert!(pasted("a/b ").is_none());
        assert!(pasted("x/y").is_none());
    }

    /// Plain characters copied out of a structure are plain characters.
    #[test]
    fn a_row_of_characters_pastes_as_characters() {
        let clip = Clip::Row(vec![Node::Char('a'), Node::Char('b')]);
        assert_eq!(clip.text(), "ab");
        assert_eq!(clip.items(), vec![vec![Item::Char('a'), Item::Char('b')]]);
    }
}
