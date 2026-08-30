//! Editing through an absolute cursor path rooted at a document line.

use super::history::Step;
use super::Editor;
use crate::structure::ast::{row_at, Cursor, Node, Row};
use crate::structure::edit::{Editing, Escape};
use crate::structure::text::{before_col, Pos, Sel};
use crate::structure::trigger::{self, Conversion};

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
        }];
        true
    }

    /// 構造内のカーソル位置にある単語を選択します。
    pub fn select_nested_word_at(&mut self, at: Pos, cursor: &Cursor) -> bool {
        if !self.enter_at(at, cursor) {
            return false;
        }
        self.with_cursor(Inside::Extend, |editing| {
            editing.select_word();
            None
        })
    }

    /// 構造内のカーソル位置があるスロット全体を選択します。
    pub fn select_nested_row_at(&mut self, at: Pos, cursor: &Cursor) -> bool {
        if !self.enter_at(at, cursor) {
            return false;
        }
        self.with_cursor(Inside::Extend, |editing| {
            editing.select_row();
            None
        })
    }

    pub fn leave_structure(&mut self) -> bool {
        let mut left = false;
        let Editor { document, cursors } = self;
        for selection in cursors {
            let Some(cursor) = selection.inside.take() else {
                continue;
            };
            let col = cursor
                .path
                .first()
                .map_or(cursor.index, |(node, _)| node + 1);
            selection.sel = Sel::caret(document.text.clamp(Pos::new(selection.sel.head.line, col)));
            left = true;
        }
        if left {
            self.document.recorder.cut();
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
        let Some(root) = self.document.text.line_mut(line) else {
            return false;
        };
        let escape = command(&mut Editing::new(root, &mut cursor));
        self.cursors[index].inside = Some(cursor);
        if let Some(escape) = escape {
            self.finish_cursor(index, line, escape);
        }
        true
    }

    fn finish_cursor(&mut self, index: usize, line: usize, escape: Escape) {
        let cursor = self.cursors[index].inside.take().unwrap_or_default();
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

    #[allow(dead_code)]
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
                    if apply_trigger(editing, c) {
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

    /// キャレットがある各 Row で、トリガー文字が構造へ移るなら変換する。
    pub fn convert_typed(&mut self, c: char) -> bool {
        if self.touches_absent() {
            return false;
        }
        if self.cursors.iter().any(|cursor| {
            !cursor.is_caret()
                || cursor
                    .inside
                    .as_ref()
                    .is_some_and(|inside| !inside.is_caret())
        }) {
            return false;
        }
        let ready: Vec<usize> = (0..self.cursors.len())
            .filter(|&index| {
                let cursor = &self.cursors[index];
                let line = self.text.line(cursor.head.line);
                let (row, col) = match &cursor.inside {
                    Some(inside) => (row_at(line, &inside.path).unwrap_or(line), inside.index),
                    None => (line, cursor.head.col),
                };
                trigger::conversion_for(row, col, c).is_some()
            })
            .collect();
        if ready.is_empty() {
            return false;
        }
        // 複数キャレットでは全員が同じトリガーを完了したときだけ変換し、それ以外は通常の文字入力にする。
        if self.cursors.len() > 1 && ready.len() != self.cursors.len() {
            return false;
        }
        self.one_step(|editor| {
            let mut processed = Vec::new();
            for &index in ready.iter().rev() {
                if editor.cursors[index].inside.is_none() {
                    let col = editor.cursors[index].head.col;
                    editor.cursors[index].inside = Some(Cursor::root(col));
                }
                let Some(before) = editor.cursors[index].inside.clone() else {
                    continue;
                };
                let line = editor.cursors[index].head.line;
                let from_root = before.path.is_empty();
                let before_len =
                    row_at(editor.text.line(line), &before.path).map(|row| row.len());
                editor.with_cursor_at(index, Inside::Change, |editing| {
                    apply_trigger(editing, c);
                    None
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
                processed.push(index);
                if !from_root {
                    continue;
                }
                match editor.cursors[index].inside.clone() {
                    Some(inside) if inside.path.is_empty() => {
                        editor.cursors[index].inside = None;
                        editor.cursors[index].sel = Sel::caret(Pos::new(line, inside.index));
                    }
                    Some(inside) => {
                        editor.cursors[index].sel = Sel::caret(Pos::new(line, inside.path[0].0));
                    }
                    None => {}
                }
            }
        });
        true
    }
}

fn apply_trigger(editing: &mut Editing<'_>, c: char) -> bool {
    let Some((consume, conversion)) = trigger::conversion_for(editing.current_row(), editing.cursor.index, c)
    else {
        return false;
    };
    match conversion {
        Conversion::Text(text) => {
            editing.convert_preceding(consume, text.chars().map(Node::char).collect(), None);
        }
        Conversion::Structure { nodes, enter } => {
            editing.convert_preceding(consume, nodes, enter);
        }
    }
    true
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
        editor.type_with_cursor(' ');
        editor.type_with_cursor('c');
        let row = editor.text().line(0);
        match &row[4].kind {
            NodeKind::Stack { above, below, .. } => {
                assert!(matches!(
                    above.as_slice(),
                    [Node {
                        kind: NodeKind::Char('b'),
                        ..
                    }]
                ));
                assert!(matches!(
                    below.as_slice(),
                    [Node {
                        kind: NodeKind::Char('c'),
                        ..
                    }]
                ));
            }
            other => panic!("expected a fraction, got {other:?}"),
        }
    }

    #[test]
    fn space_after_slash_converts_at_the_document_row() {
        let mut editor = editor("1/");
        editor.set_caret(Pos::new(0, 2));
        assert!(editor.convert_typed(' '));
        match &editor.text().line(0)[0].kind {
            NodeKind::Stack { above, .. } => {
                assert!(matches!(
                    above.as_slice(),
                    [Node {
                        kind: NodeKind::Char('1'),
                        ..
                    }]
                ));
            }
            other => panic!("expected a fraction, got {other:?}"),
        }
        assert!(editor.nested_cursor().is_some());
    }

    #[test]
    fn nested_slash_stays_text_until_space() {
        let mut editor = editor("");
        editor.start_structure();
        editor.insert_node(Node::sqrt(None, Vec::new()));
        for c in "x(x+1)".chars() {
            editor.insert_text(&c.to_string());
        }
        let body = row_at(editor.text().line(0), &[(0, 0)]).unwrap();
        assert!(body.iter().all(|node| matches!(node.kind, NodeKind::Char(_))));
    }

    #[test]
    fn structure_before_slash_converts_inside_another_structure() {
        let inner = Node::sqrt(
            None,
            "x+1".chars().map(Node::char).collect(),
        );
        let mut editor = with_rows(vec![vec![Node::sqrt(
            None,
            vec![inner.clone(), Node::char('/')],
        )]]);
        let cursor = Cursor {
            path: vec![(0, 0)],
            index: 2,
            anchor: 2,
        };
        assert!(editor.enter_at(Pos::new(0, 0), &cursor));
        assert!(editor.convert_typed(' '));
        let body = row_at(editor.text().line(0), &[(0, 0)]).unwrap();
        match &body[0].kind {
            NodeKind::Stack { above, .. } => assert_eq!(above, &vec![inner]),
            other => panic!("expected a fraction inside the root, got {other:?}"),
        }
        assert_eq!(editor.nested_cursor().unwrap().path, vec![(0, 0), (0, 1)]);
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

        };
        assert!(editor.enter_at(Pos::new(0, 0), &cursor));
        assert!(editor.add_nested(Pos::new(1, 0), cursor));
        editor.insert_text("/");
        editor.insert_text(" ");
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
