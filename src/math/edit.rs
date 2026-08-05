//! Cursor movement and editing commands on a formula.

use super::ast::{row_at, row_at_mut, Cursor, Delim, Node, Row};
use super::commands;
use super::latex::{parse_latex, row_to_latex};

const UNDO_LIMIT: usize = 200;

/// Result of an edit that the cursor could not absorb, so the surrounding text
/// editor has to react instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Escape {
    /// The caret walked off the left edge of the formula.
    Left,
    /// The caret walked off the right edge of the formula.
    Right,
    /// The formula is empty and the user pressed backspace again.
    Delete,
    /// The user asked to leave the formula (Escape / Enter).
    Done,
}

pub struct MathState {
    root: Row,
    cursor: Cursor,
    undo: Vec<(Row, Cursor)>,
    redo: Vec<(Row, Cursor)>,
}

impl MathState {
    pub fn new() -> MathState {
        MathState {
            root: Row::new(),
            cursor: Cursor::default(),
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    pub fn from_latex(latex: &str) -> MathState {
        let root = parse_latex(latex);
        let index = root.len();
        MathState {
            root,
            cursor: Cursor::root(index),
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    pub fn root(&self) -> &Row {
        &self.root
    }

    pub fn cursor(&self) -> &Cursor {
        &self.cursor
    }

    pub fn set_cursor(&mut self, cursor: Cursor) {
        if row_at(&self.root, &cursor.path).is_some_and(|r| cursor.index <= r.len()) {
            self.cursor = cursor;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.root.is_empty()
    }

    pub fn to_latex(&self) -> String {
        row_to_latex(&self.root)
    }

    fn snapshot(&mut self) {
        self.undo.push((self.root.clone(), self.cursor.clone()));
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    pub fn undo(&mut self) -> bool {
        match self.undo.pop() {
            Some((root, cursor)) => {
                self.redo
                    .push((std::mem::replace(&mut self.root, root), self.cursor.clone()));
                self.cursor = cursor;
                true
            }
            None => false,
        }
    }

    pub fn redo(&mut self) -> bool {
        match self.redo.pop() {
            Some((root, cursor)) => {
                self.undo
                    .push((std::mem::replace(&mut self.root, root), self.cursor.clone()));
                self.cursor = cursor;
                true
            }
            None => false,
        }
    }

    fn current_row(&self) -> &Row {
        row_at(&self.root, &self.cursor.path).unwrap_or(&self.root)
    }

    fn current_row_mut(&mut self) -> &mut Row {
        let path = self.cursor.path.clone();
        if row_at_mut(&mut self.root, &path).is_none() {
            self.cursor = Cursor::default();
            return &mut self.root;
        }
        row_at_mut(&mut self.root, &path).expect("row checked above")
    }

    fn node_at(&self, path: &[(usize, usize)], index: usize) -> Option<&Node> {
        row_at(&self.root, path)?.get(index)
    }

    /// Inserts a node at the caret; when the node has slots the caret moves
    /// into the first one, which is what makes palette buttons feel natural.
    pub fn insert(&mut self, node: Node) {
        self.snapshot();
        self.place(node);
    }

    fn place(&mut self, node: Node) {
        let enter = node.slot_count() > 0;
        let index = self.cursor.index;
        self.current_row_mut().insert(index, node);
        if enter {
            self.cursor.path.push((index, 0));
            self.cursor.index = 0;
        } else {
            self.cursor.index += 1;
        }
    }

    /// Turns a just-typed `\name` (or a typed glyph such as `√`) into the
    /// structure it names, the way a Markdown editor expands a shortcut.
    pub fn commit_command(&mut self) -> bool {
        let index = self.cursor.index;
        let row = self.current_row();
        let (start, node) = match command_start(row, index) {
            Some(start) => {
                let name: String = row[start + 1..index]
                    .iter()
                    .filter_map(|node| match node {
                        Node::Char(c) => Some(*c),
                        _ => None,
                    })
                    .collect();
                match commands::node_for(&name) {
                    Some(node) => (start, node),
                    None => return false,
                }
            }
            None => match row.get(index.wrapping_sub(1)) {
                Some(Node::Char(c)) => match commands::node_for_glyph(*c) {
                    Some(node) => (index - 1, node),
                    None => return false,
                },
                _ => return false,
            },
        };
        self.snapshot();
        self.current_row_mut().drain(start..index);
        self.cursor.index = start;
        self.place(node);
        true
    }

    pub fn insert_char(&mut self, c: char) {
        match c {
            '/' => self.insert_fraction(),
            '^' => self.insert(Node::Sup(Row::new())),
            '_' => self.insert(Node::Sub(Row::new())),
            '(' | '[' => self.insert(Node::Group {
                delim: Delim::from_open(c).unwrap(),
                body: Row::new(),
            }),
            ')' | ']' => self.leave_group(),
            _ => self.insert(Node::Char(c)),
        }
    }

    /// Typing `/` turns whatever was just typed into the numerator, the way a
    /// person would write the fraction on paper.
    pub fn insert_fraction(&mut self) {
        self.snapshot();
        let index = self.cursor.index;
        let start = {
            let row = self.current_row();
            numerator_start(row, index)
        };
        let num: Row = self.current_row_mut().drain(start..index).collect();
        let node = Node::Frac {
            num,
            den: Row::new(),
        };
        self.current_row_mut().insert(start, node);
        self.cursor.path.push((start, 1));
        self.cursor.index = 0;
    }

    /// Closing a delimiter moves the caret just past the group it closes.
    fn leave_group(&mut self) {
        let closes_group = self
            .cursor
            .path
            .last()
            .and_then(|&(node, _)| {
                let parent = &self.cursor.path[..self.cursor.path.len() - 1];
                self.node_at(parent, node)
            })
            .is_some_and(|node| matches!(node, Node::Group { .. }));
        if closes_group {
            let (node, _) = self.cursor.path.pop().unwrap();
            self.cursor.index = node + 1;
        }
    }

    pub fn backspace(&mut self) -> Option<Escape> {
        if self.cursor.index > 0 {
            self.snapshot();
            let index = self.cursor.index - 1;
            let row = self.current_row_mut();
            let node = row[index].clone();
            match node.slot_count() {
                // Deleting a container keeps its content: the structure is
                // peeled away instead of the user's work being thrown out.
                0 => {
                    row.remove(index);
                    self.cursor.index = index;
                }
                _ => {
                    let mut kept: Row = Vec::new();
                    for slot in 0..node.slot_count() {
                        if let Some(inner) = node.slot(slot) {
                            kept.extend(inner.iter().cloned());
                        }
                    }
                    row.remove(index);
                    let count = kept.len();
                    for (offset, inner) in kept.into_iter().enumerate() {
                        row.insert(index + offset, inner);
                    }
                    self.cursor.index = index + count;
                }
            }
            return None;
        }
        // At the start of a slot: step out of the container to its left edge.
        match self.cursor.path.pop() {
            Some((node, _)) => {
                self.cursor.index = node;
                None
            }
            None => {
                if self.root.is_empty() {
                    Some(Escape::Delete)
                } else {
                    Some(Escape::Left)
                }
            }
        }
    }

    pub fn delete_forward(&mut self) {
        let len = self.current_row().len();
        if self.cursor.index < len {
            self.snapshot();
            let index = self.cursor.index;
            self.current_row_mut().remove(index);
        }
    }

    pub fn move_left(&mut self) -> Option<Escape> {
        if self.cursor.index > 0 {
            let index = self.cursor.index - 1;
            let node = self.current_row()[index].clone();
            if node.slot_count() > 0 {
                let slot = node.exit_slot();
                let len = node.slot(slot).map(|r| r.len()).unwrap_or(0);
                self.cursor.path.push((index, slot));
                self.cursor.index = len;
            } else {
                self.cursor.index = index;
            }
            return None;
        }
        match self.cursor.path.pop() {
            Some((node, slot)) => {
                if slot > 0 {
                    let parent = self.cursor.path.clone();
                    let len = self
                        .node_at(&parent, node)
                        .and_then(|n| n.slot(slot - 1))
                        .map(|r| r.len())
                        .unwrap_or(0);
                    self.cursor.path.push((node, slot - 1));
                    self.cursor.index = len;
                } else {
                    self.cursor.index = node;
                }
                None
            }
            None => Some(Escape::Left),
        }
    }

    pub fn move_right(&mut self) -> Option<Escape> {
        let len = self.current_row().len();
        if self.cursor.index < len {
            let index = self.cursor.index;
            let node = self.current_row()[index].clone();
            if node.slot_count() > 0 {
                self.cursor.path.push((index, node.entry_slot()));
                self.cursor.index = 0;
            } else {
                self.cursor.index = index + 1;
            }
            return None;
        }
        match self.cursor.path.pop() {
            Some((node, slot)) => {
                let parent = self.cursor.path.clone();
                let slots = self
                    .node_at(&parent, node)
                    .map(|n| n.slot_count())
                    .unwrap_or(0);
                if slot + 1 < slots {
                    self.cursor.path.push((node, slot + 1));
                    self.cursor.index = 0;
                } else {
                    self.cursor.index = node + 1;
                }
                None
            }
            None => Some(Escape::Right),
        }
    }

    pub fn move_up(&mut self) -> bool {
        self.move_vertically(true)
    }

    pub fn move_down(&mut self) -> bool {
        self.move_vertically(false)
    }

    /// Walks up the path looking for a container with a slot above (or below)
    /// the current one: numerator vs denominator, upper vs lower limit, or the
    /// neighbouring row of a matrix.
    fn move_vertically(&mut self, up: bool) -> bool {
        for depth in (0..self.cursor.path.len()).rev() {
            let (node_index, slot) = self.cursor.path[depth];
            let parent_path = self.cursor.path[..depth].to_vec();
            let Some(node) = self.node_at(&parent_path, node_index).cloned() else {
                continue;
            };
            let target = match &node {
                Node::Frac { .. } => match (up, slot) {
                    (true, 1) => Some(0),
                    (false, 0) => Some(1),
                    _ => None,
                },
                Node::BigOp { .. } => match (up, slot) {
                    (true, 0) => Some(1),
                    (false, 1) => Some(0),
                    _ => None,
                },
                Node::Matrix { cells, .. } => {
                    let cols = cells.first().map(|r| r.len()).unwrap_or(1).max(1);
                    let (row, col) = (slot / cols, slot % cols);
                    if up && row > 0 {
                        Some((row - 1) * cols + col)
                    } else if !up && row + 1 < cells.len() {
                        Some((row + 1) * cols + col)
                    } else {
                        None
                    }
                }
                _ => None,
            };
            if let Some(target) = target {
                self.cursor.path.truncate(depth);
                self.cursor.path.push((node_index, target));
                self.cursor.index = node
                    .slot(target)
                    .map(|r| r.len().min(self.cursor.index))
                    .unwrap_or(0);
                return true;
            }
        }
        false
    }

    pub fn move_home(&mut self) {
        self.cursor.index = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor.index = self.current_row().len();
    }

    /// Places the caret at the very end of the formula, used when the caret
    /// enters the field from the surrounding text on the right.
    pub fn move_to_end(&mut self) {
        self.cursor = Cursor::root(self.root.len());
    }

    pub fn move_to_start(&mut self) {
        self.cursor = Cursor::root(0);
    }

    /// Adds a row (or column) to the matrix the caret is currently inside.
    pub fn grow_matrix(&mut self, add_row: bool) -> bool {
        for depth in (0..self.cursor.path.len()).rev() {
            let (node_index, slot) = self.cursor.path[depth];
            let parent_path = self.cursor.path[..depth].to_vec();
            let is_matrix = self
                .node_at(&parent_path, node_index)
                .is_some_and(|n| n.matrix_shape().is_some());
            if !is_matrix {
                continue;
            }
            self.snapshot();
            let Some(row) = row_at_mut(&mut self.root, &parent_path) else {
                return false;
            };
            let Some(Node::Matrix { cells, .. }) = row.get_mut(node_index) else {
                return false;
            };
            let cols = cells.first().map(|r| r.len()).unwrap_or(1).max(1);
            let (current_row, current_col) = (slot / cols, slot % cols);
            let target = if add_row {
                cells.insert(current_row + 1, (0..cols).map(|_| Row::new()).collect());
                (current_row + 1) * cols + current_col
            } else {
                for row in cells.iter_mut() {
                    row.insert(current_col + 1, Row::new());
                }
                current_row * (cols + 1) + current_col + 1
            };
            self.cursor.path.truncate(depth);
            self.cursor.path.push((node_index, target));
            self.cursor.index = 0;
            return true;
        }
        false
    }
}

impl Default for MathState {
    fn default() -> Self {
        MathState::new()
    }
}

/// Finds where the implicit numerator starts when `/` is typed: the run of
/// characters (or the single group) immediately before the caret.
fn numerator_start(row: &Row, index: usize) -> usize {
    if index == 0 {
        return 0;
    }
    match &row[index - 1] {
        Node::Char(c) if c.is_alphanumeric() || *c == '.' => {
            let mut start = index - 1;
            while start > 0 {
                match &row[start - 1] {
                    Node::Char(c) if c.is_alphanumeric() || *c == '.' => start -= 1,
                    _ => break,
                }
            }
            start
        }
        _ => index - 1,
    }
}

/// Finds the `\` that starts the command word ending at the caret.
fn command_start(row: &Row, index: usize) -> Option<usize> {
    let mut start = index;
    while start > 0 {
        match &row[start - 1] {
            Node::Char(c) if c.is_ascii_alphabetic() => start -= 1,
            Node::Char('\\') => return (start < index).then_some(start - 1),
            _ => return None,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_builds_a_row() {
        let mut state = MathState::new();
        for c in "x+1".chars() {
            state.insert_char(c);
        }
        assert_eq!(state.to_latex(), "x+1");
    }

    #[test]
    fn backslash_shortcut_expands_into_a_structure() {
        let mut state = MathState::new();
        for c in "\\sqrt".chars() {
            state.insert_char(c);
        }
        assert!(state.commit_command());
        state.insert_char('2');
        assert_eq!(state.to_latex(), "\\sqrt{2}");
    }

    #[test]
    fn typed_glyph_expands_like_its_command() {
        let mut state = MathState::new();
        state.insert_char('√');
        assert!(state.commit_command());
        state.insert_char('2');
        assert_eq!(state.to_latex(), "\\sqrt{2}");
    }

    #[test]
    fn unknown_backslash_shortcut_is_left_alone() {
        let mut state = MathState::new();
        for c in "\\nope".chars() {
            state.insert_char(c);
        }
        assert!(!state.commit_command());
    }

    #[test]
    fn slash_takes_the_preceding_run_as_numerator() {
        let mut state = MathState::new();
        for c in "1+ab/".chars() {
            state.insert_char(c);
        }
        for c in "2c".chars() {
            state.insert_char(c);
        }
        assert_eq!(state.to_latex(), "1+\\frac{ab}{2c}");
    }

    #[test]
    fn caret_enters_and_leaves_a_fraction() {
        let mut state = MathState::from_latex("\\frac{a}{b}");
        state.move_to_start();
        assert_eq!(state.move_right(), None);
        assert_eq!(state.cursor().path, vec![(0, 0)]);
        state.move_right();
        state.move_right();
        assert_eq!(state.cursor().path, vec![(0, 1)]);
    }

    #[test]
    fn up_and_down_switch_between_numerator_and_denominator() {
        let mut state = MathState::from_latex("\\frac{a}{b}");
        state.move_to_start();
        state.move_right();
        assert!(state.move_down());
        assert_eq!(state.cursor().path, vec![(0, 1)]);
        assert!(state.move_up());
        assert_eq!(state.cursor().path, vec![(0, 0)]);
    }

    #[test]
    fn backspace_keeps_the_content_of_a_deleted_structure() {
        let mut state = MathState::from_latex("\\frac{ab}{c}");
        state.move_to_end();
        assert_eq!(state.backspace(), None);
        assert_eq!(state.to_latex(), "abc");
    }

    #[test]
    fn backspace_reports_escape_on_an_empty_formula() {
        let mut state = MathState::new();
        assert_eq!(state.backspace(), Some(Escape::Delete));
    }

    #[test]
    fn arrow_past_the_edge_reports_escape() {
        let mut state = MathState::from_latex("x");
        state.move_to_end();
        assert_eq!(state.move_right(), Some(Escape::Right));
        state.move_to_start();
        assert_eq!(state.move_left(), Some(Escape::Left));
    }

    #[test]
    fn undo_restores_the_previous_formula() {
        let mut state = MathState::new();
        state.insert_char('a');
        state.insert_char('b');
        assert!(state.undo());
        assert_eq!(state.to_latex(), "a");
        assert!(state.redo());
        assert_eq!(state.to_latex(), "ab");
    }

    #[test]
    fn closing_paren_steps_out_of_the_group() {
        let mut state = MathState::new();
        state.insert_char('(');
        state.insert_char('x');
        state.insert_char(')');
        state.insert_char('+');
        assert_eq!(state.to_latex(), "\\left(x\\right)+");
    }

    #[test]
    fn matrix_grows_by_row_and_column() {
        let mut state = MathState::new();
        state.insert(super::super::ast::matrix(
            super::super::ast::MatrixKind::Paren,
            1,
            2,
        ));
        assert!(state.grow_matrix(true));
        match &state.root()[0] {
            Node::Matrix { cells, .. } => assert_eq!(cells.len(), 2),
            other => panic!("expected a matrix, got {other:?}"),
        }
        assert!(state.grow_matrix(false));
        match &state.root()[0] {
            Node::Matrix { cells, .. } => assert_eq!(cells[0].len(), 3),
            other => panic!("expected a matrix, got {other:?}"),
        }
    }
}
