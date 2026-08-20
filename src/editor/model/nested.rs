//! Editing through an absolute cursor path rooted at a document line.

use super::history::Step;
use super::Editor;
use crate::structure::ast::{row_at, Cursor, Node, Row};
use crate::structure::edit::{Editing, Escape};
use crate::structure::text::{before_col, Pos, Sel};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Inside {
    Move,
    Extend,
    Type,
    Change,
}

impl Editor {
    pub fn nested_cursor(&self) -> Option<&Cursor> {
        self.cursor.as_ref()
    }

    /// Begin direct editing at the top-level insertion point. No wrapper node is created.
    pub fn start_structure(&mut self) {
        if self.touches_absent() {
            return;
        }
        let at = self.primary().head;
        self.cursor = Some(Cursor::root(at.col));
        self.transient_structure = None;
        self.recorder.cut();
    }

    pub fn enter_node(&mut self, at: Pos, from_start: bool) -> bool {
        let Some(node) = self.text.node_at(at) else {
            return false;
        };
        if node.horizontal_slots().is_empty() {
            return false;
        }
        self.sels = vec![Sel::caret(at)];
        self.cursor = Some(Cursor::root(if from_start { at.col } else { at.col + 1 }));
        self.with_cursor(Inside::Move, |editing| {
            if from_start {
                editing.move_right()
            } else {
                editing.move_left()
            }
        })
    }

    pub fn enter_at(&mut self, at: Pos, cursor: &Cursor) -> bool {
        if row_at(self.text.line(at.line), &cursor.path).is_none_or(|row| cursor.index > row.len())
        {
            return false;
        }
        self.recorder.cut();
        self.sels = vec![Sel::caret(at)];
        self.cursor = Some(cursor.clone());
        true
    }

    pub fn leave_structure(&mut self) -> bool {
        let Some(cursor) = self.cursor.take() else {
            return false;
        };
        self.transient_structure = None;
        self.recorder.cut();
        let col = cursor
            .path
            .first()
            .map_or(cursor.index, |(node, _)| node + 1);
        self.sels = vec![Sel::caret(
            self.text.clamp(Pos::new(self.primary().head.line, col)),
        )];
        true
    }

    pub fn with_cursor(
        &mut self,
        kind: Inside,
        command: impl FnOnce(&mut Editing<'_>) -> Option<Escape>,
    ) -> bool {
        let Some(mut cursor) = self.cursor.clone() else {
            return false;
        };
        let line = self.primary().head.line;
        match kind {
            Inside::Move | Inside::Extend => self.recorder.cut(),
            Inside::Type => self.record(Step::Typing),
            Inside::Change => self.record(Step::Other),
        }
        let Some(root) = self.text.line_mut(line) else {
            return false;
        };
        let escape = command(&mut Editing::new(root, &mut cursor));
        let transient_done = self.transient_structure.is_some_and(|at| {
            cursor.path.is_empty()
                && root.get(at).is_some_and(|node| {
                    !matches!(
                        node.kind,
                        crate::structure::ast::NodeKind::Char(_)
                            | crate::structure::ast::NodeKind::Tab
                    ) || !node.upper.is_empty()
                        || !node.lower.is_empty()
                })
        });
        self.cursor = Some(cursor);
        if let Some(escape) = escape {
            self.finish_cursor(line, escape);
        } else if transient_done {
            let col = self.cursor.as_ref().map_or(0, |cursor| cursor.index);
            self.cursor = None;
            self.transient_structure = None;
            self.sels = vec![Sel::caret(Pos::new(line, col))];
        }
        true
    }

    fn finish_cursor(&mut self, line: usize, escape: Escape) {
        let cursor = self.cursor.take().unwrap_or_default();
        self.transient_structure = None;
        let col = match escape {
            Escape::Left | Escape::Delete => 0,
            Escape::Right => cursor.index.min(self.text.line_len(line)),
        };
        self.sels = vec![Sel::caret(Pos::new(line, col))];
    }

    pub fn select_nested(&mut self, at: Pos, cursor: Cursor) -> bool {
        self.enter_at(at, &cursor)
    }

    pub fn replace_nested(&mut self, at: Pos, cursor: Cursor, with: &str) -> bool {
        if self.touches_absent() {
            return false;
        }
        if !self.select_nested(at, cursor) {
            return false;
        }
        let nodes = with
            .chars()
            .filter(|c| *c != '\n' && *c != '\t')
            .map(Node::char)
            .collect();
        self.insert_nested_row(nodes)
    }

    pub fn select_structure(&mut self) -> bool {
        let Some(cursor) = self.cursor.take() else {
            return false;
        };
        let Some((node, _)) = cursor.path.first().copied() else {
            return false;
        };
        self.recorder.cut();
        let line = self.primary().head.line;
        self.sels = vec![Sel::range(Pos::new(line, node), Pos::new(line, node + 1))];
        true
    }

    pub fn extend_nested(&mut self, cursor: &Cursor) -> bool {
        let to = cursor.clone();
        self.with_cursor(Inside::Extend, move |editing| {
            editing.extend_to(&to);
            None
        })
    }

    pub fn nested_selection(&self) -> Option<Row> {
        let cursor = self.cursor.as_ref()?;
        if cursor.is_caret() {
            return None;
        }
        let row = row_at(self.text.line(self.primary().head.line), &cursor.path)?;
        Some(row[cursor.start()..cursor.end().min(row.len())].to_vec())
    }

    pub fn insert_node(&mut self, node: Node) -> bool {
        if self.touches_absent() {
            return false;
        }
        self.with_cursor(Inside::Change, |editing| {
            editing.insert(node);
            None
        })
    }

    pub fn insert_nested_row(&mut self, nodes: Row) -> bool {
        if self.touches_absent() {
            return false;
        }
        self.with_cursor(Inside::Change, |editing| {
            editing.insert_row(nodes);
            None
        })
    }

    pub(super) fn type_with_cursor(&mut self, c: char) -> bool {
        let mut escaped = false;
        let done = self.with_cursor(Inside::Type, |editing| {
            if c == ' ' && editing.commit_command() {
                return None;
            }
            let result = editing.insert_char(c);
            escaped = result.is_some();
            result
        });
        if done && escaped {
            let mut buffer = [0u8; 4];
            self.insert_text(c.encode_utf8(&mut buffer));
        }
        done
    }

    pub fn limit_trigger_to_structure(&mut self) {
        self.transient_structure = self.cursor.as_ref().map(|cursor| cursor.index);
    }

    pub(super) fn enter_node_beside(&mut self, forward: bool) -> bool {
        let sel = self.primary();
        if !sel.is_caret() || self.sels.len() != 1 {
            return false;
        }
        let at = if forward {
            Some(sel.head)
        } else {
            before_col(sel.head)
        };
        at.is_some_and(|at| self.enter_node(at, forward))
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::editor;
    use super::*;
    use crate::structure::ast::{Between, NodeKind};

    #[test]
    fn a_fraction_is_inserted_directly_among_document_characters() {
        let mut editor = editor("a + b");
        editor.set_caret(Pos::new(0, 5));
        editor.start_structure();
        editor.type_with_cursor('/');
        editor.type_with_cursor('c');
        editor.type_with_cursor(' ');
        editor.insert_text("+ d");
        let row = editor.text().line(0);
        assert!(matches!(row[4].kind, NodeKind::Stack { .. }));
        assert!(matches!(row[5].kind, NodeKind::Char(' ')));
    }

    #[test]
    fn a_trigger_tracks_the_new_structure_in_a_row_with_existing_structures() {
        let mut editor = editor("");
        editor.insert(vec![vec![Node::sqrt(None, Vec::new()), Node::char(' ')]]);
        editor.start_structure();
        editor.limit_trigger_to_structure();
        for c in "b/c+d".chars() {
            editor.insert_text(&c.to_string());
        }
        let row = editor.text().line(0);
        assert!(matches!(row[0].kind, NodeKind::Sqrt { .. }));
        assert!(matches!(row[2].kind, NodeKind::Stack { .. }));
        assert!(matches!(row[3].kind, NodeKind::Char('+')));
        assert!(matches!(row[4].kind, NodeKind::Char('d')));
    }

    #[test]
    fn nested_rows_reject_document_line_breaks() {
        let mut editor = editor("");
        editor.start_structure();
        editor.insert_node(Node::stack(Vec::new(), Vec::new(), Between::Rule));
        editor.insert_text("a\nb");
        assert_eq!(editor.text().line_count(), 1);
        let expected = vec![Node::char('a'), Node::char('b')];
        assert_eq!(
            row_at(editor.text().line(0), &[(0, 0)]),
            Some(expected.as_slice())
        );
        assert!(matches!(editor.split_line(), super::super::Did::Moved));
        assert_eq!(editor.text().line_count(), 1);
        assert!(editor.nested_cursor().is_none());
    }

    #[test]
    fn nested_replacement_drops_line_and_column_separators() {
        let mut editor = editor("");
        editor.insert(vec![vec![Node::stack(
            vec![Node::char('x')],
            Vec::new(),
            Between::Rule,
        )]]);
        assert!(editor.replace_nested(
            Pos::new(0, 0),
            Cursor {
                path: vec![(0, 0)],
                index: 1,
                anchor: 0,
                fills: Vec::new(),
            },
            "a\nb\tc",
        ));
        let expected = vec![Node::char('a'), Node::char('b'), Node::char('c')];
        assert_eq!(
            row_at(editor.text().line(0), &[(0, 0)]),
            Some(expected.as_slice())
        );
    }

    #[test]
    fn nested_fraction_and_root_paths_are_absolute() {
        let mut editor = editor("");
        editor.start_structure();
        editor.insert_node(Node::stack(Vec::new(), Vec::new(), Between::Rule));
        assert_eq!(editor.nested_cursor().unwrap().path, vec![(0, 0)]);
        editor.insert_node(Node::sqrt(None, Vec::new()));
        assert_eq!(editor.nested_cursor().unwrap().path, vec![(0, 0), (0, 0)]);
    }
}
