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
        self.primary_cursor().inside.as_ref()
    }

    /// Begin direct editing at the top-level insertion point. No wrapper node is created.
    pub fn start_structure(&mut self) {
        if self.touches_absent() {
            return;
        }
        for cursor in &mut self.cursors {
            cursor.inside = Some(Cursor::root(cursor.head.col));
            cursor.transient_structure = None;
        }
        self.recorder.cut();
    }

    pub fn enter_node(&mut self, at: Pos, from_start: bool) -> bool {
        let Some(node) = self.text.node_at(at) else {
            return false;
        };
        if node.horizontal_slots().is_empty() {
            return false;
        }
        self.cursors = vec![super::UnifiedCursor {
            sel: Sel::caret(at),
            inside: Some(Cursor::root(if from_start { at.col } else { at.col + 1 })),
            transient_structure: None,
        }];
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
        self.cursors = vec![super::UnifiedCursor {
            sel: Sel::caret(at),
            inside: Some(cursor.clone()),
            transient_structure: None,
        }];
        true
    }

    pub fn leave_structure(&mut self) -> bool {
        let mut left = false;
        for selection in &mut self.cursors {
            let Some(cursor) = selection.inside.take() else {
                continue;
            };
            selection.transient_structure = None;
            let col = cursor
                .path
                .first()
                .map_or(cursor.index, |(node, _)| node + 1);
            selection.sel = Sel::caret(self.text.clamp(Pos::new(selection.sel.head.line, col)));
            left = true;
        }
        if left {
            self.recorder.cut();
        }
        left
    }

    pub fn with_cursor(
        &mut self,
        kind: Inside,
        command: impl FnOnce(&mut Editing<'_>) -> Option<Escape>,
    ) -> bool {
        let index = self.cursors.len() - 1;
        self.with_cursor_at(index, kind, command)
    }

    pub(super) fn with_each_cursor(
        &mut self,
        kind: Inside,
        mut command: impl FnMut(&mut Editing<'_>) -> Option<Escape>,
    ) -> bool {
        let indices: Vec<usize> = self
            .cursors
            .iter()
            .enumerate()
            .filter_map(|(index, cursor)| cursor.inside.is_some().then_some(index))
            .rev()
            .collect();
        let mut done = false;
        self.one_step(|editor| {
            let mut processed = Vec::new();
            for index in indices {
                let Some(before) = editor.cursors[index].inside.clone() else {
                    continue;
                };
                let line = editor.cursors[index].head.line;
                let before_len = row_at(editor.text.line(line), &before.path).map(|row| row.len());
                done |= editor.with_cursor_at(index, kind, |editing| command(editing));
                let after_len = row_at(editor.text.line(line), &before.path).map(|row| row.len());
                if let (Some(before_len), Some(after_len)) = (before_len, after_len) {
                    let delta = after_len as isize - before_len as isize;
                    if delta != 0 {
                        editor.shift_after_nested_edit(
                            &processed,
                            line,
                            &before.path,
                            before.index,
                            delta,
                        );
                    }
                }
                processed.push(index);
            }
        });
        done
    }

    fn shift_after_nested_edit(
        &mut self,
        processed: &[usize],
        line: usize,
        path: &[(usize, usize)],
        at: usize,
        delta: isize,
    ) {
        let shift = |value: usize| {
            if value <= at {
                value
            } else if delta >= 0 {
                value.saturating_add(delta as usize)
            } else {
                value.saturating_sub((-delta) as usize)
            }
        };
        for &index in processed {
            let selection = &mut self.cursors[index];
            if selection.head.line != line {
                continue;
            }
            let Some(cursor) = selection.inside.as_mut() else {
                continue;
            };
            if cursor.path == path {
                cursor.index = shift(cursor.index);
                cursor.anchor = shift(cursor.anchor);
            } else if cursor.path.starts_with(path) && cursor.path.len() > path.len() {
                cursor.path[path.len()].0 = shift(cursor.path[path.len()].0);
            }
            if path.is_empty() {
                selection.sel.anchor.col = shift(selection.sel.anchor.col);
                selection.sel.head.col = shift(selection.sel.head.col);
            }
        }
    }

    fn with_cursor_at(
        &mut self,
        index: usize,
        kind: Inside,
        command: impl FnOnce(&mut Editing<'_>) -> Option<Escape>,
    ) -> bool {
        let Some(mut cursor) = self.cursors[index].inside.clone() else {
            return false;
        };
        let line = self.cursors[index].head.line;
        match kind {
            Inside::Move | Inside::Extend => self.recorder.cut(),
            Inside::Type => {
                self.record(Step::Typing);
                self.modified_lines.insert(line);
            }
            Inside::Change => {
                self.record(Step::Other);
                self.modified_lines.insert(line);
            }
        }
        let Some(root) = self.text.line_mut(line) else {
            return false;
        };
        let escape = command(&mut Editing::new(root, &mut cursor));
        let transient_done = self.cursors[index].transient_structure.is_some_and(|at| {
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
        self.cursors[index].inside = Some(cursor);
        if let Some(escape) = escape {
            self.finish_cursor(index, line, escape);
        } else if transient_done {
            let col = self.cursors[index]
                .inside
                .as_ref()
                .map_or(0, |cursor| cursor.index);
            self.cursors[index].inside = None;
            self.cursors[index].transient_structure = None;
            self.cursors[index].sel = Sel::caret(Pos::new(line, col));
        }
        true
    }

    fn finish_cursor(&mut self, index: usize, line: usize, escape: Escape) {
        let cursor = self.cursors[index].inside.take().unwrap_or_default();
        self.cursors[index].transient_structure = None;
        let col = match escape {
            Escape::Left | Escape::Delete => 0,
            Escape::Right => cursor.index.min(self.text.line_len(line)),
        };
        self.cursors[index].sel = Sel::caret(Pos::new(line, col));
    }

    pub fn select_nested(&mut self, at: Pos, cursor: Cursor) -> bool {
        self.enter_at(at, &cursor)
    }

    pub fn add_nested(&mut self, at: Pos, cursor: Cursor) -> bool {
        if row_at(self.text.line(at.line), &cursor.path).is_none_or(|row| cursor.index > row.len())
        {
            return false;
        }
        self.recorder.cut();
        self.cursors.push(super::UnifiedCursor {
            sel: Sel::caret(at),
            inside: Some(cursor),
            transient_structure: None,
        });
        self.merge_sels();
        true
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
        let Some(cursor) = self.primary_cursor_mut().inside.take() else {
            return false;
        };
        let Some((node, _)) = cursor.path.first().copied() else {
            return false;
        };
        self.recorder.cut();
        let line = self.primary().head.line;
        self.cursors = vec![super::UnifiedCursor::range(
            Pos::new(line, node),
            Pos::new(line, node + 1),
        )];
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
        let cursor = self.primary_cursor().inside.as_ref()?;
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
        let indices: Vec<usize> = self
            .cursors
            .iter()
            .enumerate()
            .filter_map(|(index, cursor)| cursor.inside.is_some().then_some(index))
            .rev()
            .collect();
        let mut done = false;
        self.one_step(|editor| {
            for index in indices {
                let node = node.clone();
                done |= editor.with_cursor_at(index, Inside::Change, |editing| {
                    editing.insert(node);
                    None
                });
            }
        });
        done
    }

    pub fn insert_nested_row(&mut self, nodes: Row) -> bool {
        if self.touches_absent() {
            return false;
        }
        let indices: Vec<usize> = self
            .cursors
            .iter()
            .enumerate()
            .filter_map(|(index, cursor)| cursor.inside.is_some().then_some(index))
            .rev()
            .collect();
        let mut done = false;
        self.one_step(|editor| {
            for index in indices {
                let nodes = nodes.clone();
                done |= editor.with_cursor_at(index, Inside::Change, |editing| {
                    editing.insert_row(nodes);
                    None
                });
            }
        });
        done
    }

    pub(super) fn type_with_cursor(&mut self, c: char) -> bool {
        let indices: Vec<usize> = self
            .cursors
            .iter()
            .enumerate()
            .filter_map(|(index, cursor)| cursor.inside.is_some().then_some(index))
            .rev()
            .collect();
        let mut done = false;
        let mut escaped = Vec::new();
        self.one_step(|editor| {
            let mut processed = Vec::new();
            for index in indices {
                let Some(before) = editor.cursors[index].inside.clone() else {
                    continue;
                };
                let line = editor.cursors[index].head.line;
                let before_len = row_at(editor.text.line(line), &before.path).map(|row| row.len());
                let mut left = false;
                done |= editor.with_cursor_at(index, Inside::Type, |editing| {
                    if c == ' ' && editing.commit_command() {
                        return None;
                    }
                    let escape = editing.insert_char(c);
                    left = escape.is_some();
                    escape
                });
                let after_len = row_at(editor.text.line(line), &before.path).map(|row| row.len());
                if let (Some(before_len), Some(after_len)) = (before_len, after_len) {
                    let delta = after_len as isize - before_len as isize;
                    if delta != 0 {
                        editor.shift_after_nested_edit(
                            &processed,
                            line,
                            &before.path,
                            before.index,
                            delta,
                        );
                    }
                }
                if left {
                    escaped.push(index);
                }
                processed.push(index);
            }
        });
        if !escaped.is_empty() {
            let mut buffer = [0u8; 4];
            self.insert_indices(
                vec![c.encode_utf8(&mut buffer).chars().map(Node::char).collect()],
                escaped,
            );
        }
        done
    }

    pub fn limit_trigger_to_structure(&mut self) {
        let transient = self
            .primary_cursor()
            .inside
            .as_ref()
            .map(|cursor| cursor.index);
        self.primary_cursor_mut().transient_structure = transient;
    }

    pub(super) fn move_vertical_cursors(&mut self, down: bool) {
        let indices: Vec<usize> = self
            .cursors
            .iter()
            .enumerate()
            .filter_map(|(index, cursor)| cursor.inside.is_some().then_some(index))
            .rev()
            .collect();
        for index in indices {
            let mut moved = false;
            self.with_cursor_at(index, Inside::Move, |editing| {
                moved = if down {
                    editing.move_down()
                } else {
                    editing.move_up()
                };
                None
            });
            if !moved {
                self.cursors[index].inside = None;
                self.cursors[index].transient_structure = None;
            }
        }
    }

    pub(super) fn enter_node_beside(&mut self, forward: bool) -> bool {
        let sel = self.primary();
        if !sel.is_caret() || self.cursors.len() != 1 {
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
    use super::super::tests::{editor, with_rows};
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
    fn multiple_nested_cursors_edit_different_structure_rows() {
        let mut editor = with_rows(vec![
            vec![Node::sqrt(None, Vec::new())],
            vec![Node::sqrt(None, Vec::new())],
        ]);
        let cursor = Cursor {
            path: vec![(0, 0)],
            index: 0,
            anchor: 0,
            fills: Vec::new(),
        };
        assert!(editor.enter_at(Pos::new(0, 0), &cursor));
        assert!(editor.add_nested(Pos::new(1, 0), cursor));
        editor.insert_text("X");
        for line in 0..2 {
            assert_eq!(
                row_at(editor.text().line(line), &[(0, 0)]),
                Some([Node::char('X')].as_slice())
            );
        }
    }

    #[test]
    fn multiple_nested_cursors_apply_structure_triggers() {
        let mut editor = with_rows(vec![
            vec![Node::sqrt(None, vec![Node::char('a')])],
            vec![Node::sqrt(None, vec![Node::char('b')])],
        ]);
        let cursor = Cursor {
            path: vec![(0, 0)],
            index: 1,
            anchor: 1,
            fills: Vec::new(),
        };
        assert!(editor.enter_at(Pos::new(0, 0), &cursor));
        assert!(editor.add_nested(Pos::new(1, 0), cursor));
        editor.insert_text("/");
        for line in 0..2 {
            let row = row_at(editor.text().line(line), &[(0, 0)]).unwrap();
            assert!(matches!(row[0].kind, NodeKind::Stack { .. }));
        }
    }

    #[test]
    fn two_cursors_in_one_nested_row_keep_their_positions_after_typing() {
        let mut editor = with_rows(vec![vec![Node::sqrt(
            None,
            "a b".chars().map(Node::char).collect(),
        )]]);
        let first = Cursor {
            path: vec![(0, 0)],
            index: 1,
            anchor: 1,
            fills: Vec::new(),
        };
        let second = Cursor {
            index: 3,
            anchor: 3,
            ..first.clone()
        };
        assert!(editor.enter_at(Pos::new(0, 0), &first));
        assert!(editor.add_nested(Pos::new(0, 0), second));
        editor.insert_text("X");
        editor.insert_text("Y");
        assert_eq!(
            row_at(editor.text().line(0), &[(0, 0)]),
            Some(
                "aXY bXY"
                    .chars()
                    .map(Node::char)
                    .collect::<Vec<_>>()
                    .as_slice()
            )
        );
    }

    #[test]
    fn tab_moves_every_nested_cursor_to_the_next_slot() {
        let mut editor = with_rows(vec![
            vec![Node::stack(Vec::new(), Vec::new(), Between::Rule)],
            vec![Node::stack(Vec::new(), Vec::new(), Between::Rule)],
        ]);
        let cursor = Cursor {
            path: vec![(0, 0)],
            index: 0,
            anchor: 0,
            fills: Vec::new(),
        };
        assert!(editor.enter_at(Pos::new(0, 0), &cursor));
        assert!(editor.add_nested(Pos::new(1, 0), cursor));
        assert!(matches!(editor.tab(false), super::super::Did::Moved));
        assert!(editor.cursors().iter().all(|selection| {
            selection
                .inside
                .as_ref()
                .is_some_and(|cursor| cursor.path == vec![(0, 1)])
        }));
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
