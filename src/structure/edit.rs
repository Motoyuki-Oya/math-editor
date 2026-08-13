//! Cursor movement and editing commands inside an island.

use super::ast::{is_arrow, row_at, row_at_mut, Between, Cursor, Delim, Node, Row};
use super::commands;

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

/// An island being edited: the structure itself, borrowed from the document,
/// and the cursor walking through it.
///
/// Nothing is copied and no history is kept here. The island belongs to the
/// document, so an edit inside one is an edit of the document, undone by the
/// same history as any other.
pub struct Editing<'a> {
    pub root: &'a mut Row,
    pub cursor: &'a mut Cursor,
}

impl<'a> Editing<'a> {
    pub fn new(root: &'a mut Row, cursor: &'a mut Cursor) -> Editing<'a> {
        Editing { root, cursor }
    }

    pub fn set_cursor(&mut self, cursor: Cursor) {
        if row_at(self.root, &cursor.path).is_some_and(|r| cursor.index <= r.len()) {
            *self.cursor = cursor;
        }
    }

    fn current_row(&self) -> &Row {
        row_at(self.root, &self.cursor.path).unwrap_or(self.root)
    }

    fn current_row_mut(&mut self) -> &mut Row {
        let path = self.cursor.path.clone();
        if row_at_mut(self.root, &path).is_none() {
            *self.cursor = Cursor::default();
            return self.root;
        }
        row_at_mut(self.root, &path).expect("row checked above")
    }

    fn node_at(&self, path: &[(usize, usize)], index: usize) -> Option<&Node> {
        row_at(self.root, path)?.get(index)
    }

    /// Inserts a node at the caret; when the node has slots the caret moves
    /// into the first one, which is what makes palette buttons feel natural.
    pub fn insert(&mut self, node: Node) {
        self.place(node);
    }

    /// Grows the selection by one place, or takes the whole structure the
    /// selection sits in once it reaches the end of its row.
    pub fn extend(&mut self, forward: bool) -> Option<Escape> {
        let index = self.cursor.index;
        let target = if forward {
            (index < self.current_row().len()).then_some(index + 1)
        } else {
            index.checked_sub(1)
        };
        match target {
            Some(index) => {
                self.cursor.index = index;
                None
            }
            None => self.select_around(forward),
        }
    }

    /// Moves the head of the selection to `to`, which is how dragging selects.
    /// A place in another row is not part of the same selection, so it is left
    /// alone rather than guessed at.
    pub fn extend_to(&mut self, to: &Cursor) {
        if to.path != self.cursor.path {
            return;
        }
        self.cursor.index = to.index.min(self.current_row().len());
    }

    /// Selects the structure the caret is inside, in the row that holds it.
    /// Selecting therefore keeps widening: character, the structure around it,
    /// the structure around that one, and finally the whole formula, which is
    /// where the surrounding text takes over.
    fn select_around(&mut self, forward: bool) -> Option<Escape> {
        let Some((node, _)) = self.cursor.path.pop() else {
            return Some(if forward { Escape::Right } else { Escape::Left });
        };
        let (anchor, index) = if forward {
            (node, node + 1)
        } else {
            (node + 1, node)
        };
        self.cursor.anchor = anchor;
        self.cursor.index = index;
        None
    }

    /// Selects the whole row the caret is in, for Select All.
    pub fn select_row(&mut self) {
        self.cursor.anchor = 0;
        self.cursor.index = self.current_row().len();
    }

    /// Puts a plain caret at `index`. Everything except selecting ends this
    /// way, so a selection never outlives the command that made it.
    fn caret_at(&mut self, index: usize) {
        self.cursor.index = index;
        self.cursor.anchor = index;
    }

    /// A selection collapses when the caret moves, the way it does in text: to
    /// the side the move goes towards.
    fn collapse(&mut self, forward: bool) -> bool {
        if self.cursor.is_caret() {
            return false;
        }
        let index = if forward {
            self.cursor.end()
        } else {
            self.cursor.start()
        };
        self.caret_at(index);
        true
    }

    /// Drops the selected structures, so typing over a selection replaces it.
    fn take_selection(&mut self) -> bool {
        if self.cursor.is_caret() {
            return false;
        }
        let (start, end) = (self.cursor.start(), self.cursor.end());
        self.current_row_mut().drain(start..end);
        self.caret_at(start);
        true
    }

    /// Puts a whole row of structures in at the caret, for a paste.
    pub fn insert_row(&mut self, nodes: Row) {
        self.take_selection();
        let index = self.cursor.index;
        let count = nodes.len();
        let row = self.current_row_mut();
        for (offset, node) in nodes.into_iter().enumerate() {
            row.insert(index + offset, node);
        }
        self.caret_at(index + count);
    }

    fn place(&mut self, node: Node) {
        self.take_selection();
        let enter = node.slot_count() > 0;
        let index = self.cursor.index;
        self.current_row_mut().insert(index, node);
        if enter {
            self.cursor.path.push((index, 0));
            self.caret_at(0);
        } else {
            self.caret_at(self.cursor.index + 1);
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
        self.current_row_mut().drain(start..index);
        self.caret_at(start);
        self.place(node);
        true
    }

    pub fn insert_char(&mut self, c: char) {
        match c {
            '/' => self.insert_stack(Between::Rule),
            c if is_arrow(c) => self.insert_stack(Between::Arrow(c)),
            '^' => self.insert(Node::Sup(Row::new())),
            '_' => self.insert(Node::Sub(Row::new())),
            '(' | '[' => self.insert(Node::Group {
                delim: Delim::from_open(c).unwrap(),
                body: Row::new(),
            }),
            ')' | ']' => self.leave_group(),
            // A grid grows by a column where the caret is; anywhere else `&` is
            // just a character.
            '&' => {
                if !self.grow_matrix(false) {
                    self.insert(Node::Char('&'));
                }
            }
            _ => self.insert(Node::Char(c)),
        }
    }

    /// Typing `/` (or an arrow) puts whatever was just typed above it, the way
    /// a person would write it on paper.
    pub fn insert_stack(&mut self, between: Between) {
        self.take_selection();
        let index = self.cursor.index;
        let start = {
            let row = self.current_row();
            above_start(row, index)
        };
        let above: Row = self.current_row_mut().drain(start..index).collect();
        let node = Node::Stack {
            above,
            below: Row::new(),
            between,
        };
        self.current_row_mut().insert(start, node);
        self.cursor.path.push((start, 1));
        self.caret_at(0);
    }

    /// Closing a delimiter moves the caret just past the group it closes, which
    /// is the innermost bracket the caret is anywhere inside of: closing from a
    /// denominator inside the brackets closes them all the same, the way it
    /// would if the whole thing had been typed on one line.
    fn leave_group(&mut self) {
        let mut depth = self.cursor.path.len();
        while depth > 0 {
            let (node, _) = self.cursor.path[depth - 1];
            let parent = &self.cursor.path[..depth - 1];
            if matches!(self.node_at(parent, node), Some(Node::Group { .. })) {
                self.cursor.path.truncate(depth - 1);
                self.caret_at(node + 1);
                return;
            }
            depth -= 1;
        }
    }

    pub fn backspace(&mut self) -> Option<Escape> {
        if self.take_selection() {
            return None;
        }
        if self.cursor.index > 0 {
            let index = self.cursor.index - 1;
            let row = self.current_row_mut();
            let node = row[index].clone();
            match node.slot_count() {
                // Deleting a container keeps its content: the structure is
                // peeled away instead of the user's work being thrown out.
                0 => {
                    row.remove(index);
                    self.caret_at(index);
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
                    self.caret_at(index + count);
                }
            }
            return None;
        }
        // At the start of a slot: step out of the container to its left edge.
        match self.cursor.path.pop() {
            Some((node, _)) => {
                self.caret_at(node);
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
        if self.take_selection() {
            return;
        }
        let len = self.current_row().len();
        if self.cursor.index < len {
            let index = self.cursor.index;
            self.current_row_mut().remove(index);
        }
    }

    pub fn move_left(&mut self) -> Option<Escape> {
        // Moving off a selection just puts the caret at its edge.
        if self.collapse(false) {
            return None;
        }
        if self.cursor.index > 0 {
            let index = self.cursor.index - 1;
            let node = self.current_row()[index].clone();
            if node.slot_count() > 0 {
                let slot = node.exit_slot();
                let len = node.slot(slot).map(|r| r.len()).unwrap_or(0);
                self.cursor.path.push((index, slot));
                self.caret_at(len);
            } else {
                self.caret_at(index);
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
                    self.caret_at(len);
                } else {
                    self.caret_at(node);
                }
                None
            }
            None => Some(Escape::Left),
        }
    }

    pub fn move_right(&mut self) -> Option<Escape> {
        if self.collapse(true) {
            return None;
        }
        let len = self.current_row().len();
        if self.cursor.index < len {
            let index = self.cursor.index;
            let node = self.current_row()[index].clone();
            if node.slot_count() > 0 {
                self.cursor.path.push((index, node.entry_slot()));
                self.caret_at(0);
            } else {
                self.caret_at(index + 1);
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
                    self.caret_at(0);
                } else {
                    self.caret_at(node + 1);
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
        self.collapse(!up);
        for depth in (0..self.cursor.path.len()).rev() {
            let (node_index, slot) = self.cursor.path[depth];
            let parent_path = self.cursor.path[..depth].to_vec();
            let Some(node) = self.node_at(&parent_path, node_index).cloned() else {
                continue;
            };
            let target = match &node {
                Node::Stack { .. } => match (up, slot) {
                    (true, 1) => Some(0),
                    (false, 0) => Some(1),
                    _ => None,
                },
                Node::Limits { .. } => match (up, slot) {
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
                let index = node
                    .slot(target)
                    .map(|r| r.len().min(self.cursor.index))
                    .unwrap_or(0);
                self.caret_at(index);
                return true;
            }
        }
        false
    }

    pub fn move_home(&mut self) {
        self.caret_at(0);
    }

    pub fn move_end(&mut self) {
        self.caret_at(self.current_row().len());
    }

    /// Places the caret at the very end of the formula, used when the caret
    /// enters the field from the surrounding text on the right.
    pub fn move_to_end(&mut self) {
        *self.cursor = Cursor::root(self.root.len());
    }

    pub fn move_to_start(&mut self) {
        *self.cursor = Cursor::root(0);
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
            let Some(row) = row_at_mut(self.root, &parent_path) else {
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
            self.caret_at(0);
            return true;
        }
        false
    }
}

/// Finds where the implicit upper row starts when `/` is typed: the run of
/// characters (or the single group) immediately before the caret.
fn above_start(row: &Row, index: usize) -> usize {
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
    /// Only the fixtures here go through the notation; nothing outside a test
    /// in this layer may know how a structure is written.
    use crate::format::notation;

    /// An island on its own, standing in for the document that would hold it.
    struct Island {
        root: Row,
        cursor: Cursor,
    }

    impl Island {
        fn new() -> Island {
            Island {
                root: Row::new(),
                cursor: Cursor::default(),
            }
        }

        fn from_notation(source: &str) -> Island {
            let root = notation::parse_island(source);
            let cursor = Cursor::root(root.len());
            Island { root, cursor }
        }

        fn edit(&mut self) -> Editing<'_> {
            Editing::new(&mut self.root, &mut self.cursor)
        }

        fn type_in(&mut self, text: &str) {
            for c in text.chars() {
                self.edit().insert_char(c);
            }
        }

        fn to_notation(&self) -> String {
            notation::island_text(&self.root)
        }
    }

    #[test]
    fn typing_builds_a_row() {
        let mut island = Island::new();
        island.type_in("x+1");
        assert_eq!(island.to_notation(), "x+1");
    }

    #[test]
    fn backslash_shortcut_expands_into_a_structure() {
        let mut island = Island::new();
        island.type_in("\\sqrt");
        assert!(island.edit().commit_command());
        island.type_in("2");
        assert_eq!(island.to_notation(), "√ 2");
    }

    #[test]
    fn typed_glyph_expands_like_its_command() {
        let mut island = Island::new();
        island.type_in("√");
        assert!(island.edit().commit_command());
        island.type_in("2");
        assert_eq!(island.to_notation(), "√ 2");
    }

    #[test]
    fn unknown_backslash_shortcut_is_left_alone() {
        let mut island = Island::new();
        island.type_in("\\nope");
        assert!(!island.edit().commit_command());
    }

    #[test]
    fn slash_takes_the_preceding_run_as_the_upper_row() {
        let mut island = Island::new();
        island.type_in("1+ab/");
        island.type_in("2c");
        assert_eq!(island.to_notation(), "1+$(ab/2c)");
    }

    #[test]
    fn a_closing_bracket_leaves_the_brackets_from_inside_a_fraction() {
        let mut island = Island::new();
        island.type_in("1/(2/3)+4");
        // `+4` follows the fraction instead of falling into its lower row.
        assert_eq!(island.to_notation(), "1/($(2/3))+4");
    }

    #[test]
    fn a_closing_bracket_with_no_brackets_open_changes_nothing() {
        let mut island = Island::new();
        island.type_in("a)b");
        assert_eq!(island.to_notation(), "ab");
    }

    #[test]
    fn caret_enters_and_leaves_a_stack() {
        let mut island = Island::from_notation("a/b");
        island.edit().move_to_start();
        assert_eq!(island.edit().move_right(), None);
        assert_eq!(island.cursor.path, vec![(0, 0)]);
        island.edit().move_right();
        island.edit().move_right();
        assert_eq!(island.cursor.path, vec![(0, 1)]);
    }

    #[test]
    fn up_and_down_switch_between_the_upper_and_lower_row() {
        let mut island = Island::from_notation("a/b");
        island.edit().move_to_start();
        island.edit().move_right();
        assert!(island.edit().move_down());
        assert_eq!(island.cursor.path, vec![(0, 1)]);
        assert!(island.edit().move_up());
        assert_eq!(island.cursor.path, vec![(0, 0)]);
    }

    #[test]
    fn backspace_keeps_the_content_of_a_deleted_structure() {
        let mut island = Island::from_notation("ab/c");
        island.edit().move_to_end();
        assert_eq!(island.edit().backspace(), None);
        assert_eq!(island.to_notation(), "abc");
    }

    #[test]
    fn backspace_reports_escape_on_an_empty_formula() {
        let mut island = Island::new();
        assert_eq!(island.edit().backspace(), Some(Escape::Delete));
    }

    #[test]
    fn arrow_past_the_edge_reports_escape() {
        let mut island = Island::from_notation("x");
        island.edit().move_to_end();
        assert_eq!(island.edit().move_right(), Some(Escape::Right));
        island.edit().move_to_start();
        assert_eq!(island.edit().move_left(), Some(Escape::Left));
    }

    #[test]
    fn closing_paren_steps_out_of_the_group() {
        let mut island = Island::new();
        island.type_in("(x)+");
        assert_eq!(island.to_notation(), "(x)+");
    }

    #[test]
    fn selecting_reaches_the_whole_row_then_the_structure_around_it() {
        let mut island = Island::from_notation("1/2");
        island.edit().move_to_end();
        island.edit().move_left();
        // Moving into the fraction from the right lands in its lower row.
        assert_eq!(island.cursor.path, vec![(0, 1)]);
        assert_eq!(island.edit().extend(false), None);
        assert_eq!((island.cursor.start(), island.cursor.end()), (0, 1));
        // Reaching past the row selects the fraction, in the row above it.
        assert_eq!(island.edit().extend(false), None);
        assert_eq!(island.cursor.path, Vec::new());
        assert_eq!((island.cursor.start(), island.cursor.end()), (0, 1));
        // And past the outermost row, the selection leaves the formula.
        assert_eq!(island.edit().extend(false), Some(Escape::Left));
    }

    #[test]
    fn select_row_takes_everything_in_the_row() {
        let mut island = Island::from_notation("ab");
        island.edit().select_row();
        assert_eq!((island.cursor.start(), island.cursor.end()), (0, 2));
        island.edit().backspace();
        assert_eq!(island.to_notation(), "");
        assert!(island.cursor.is_caret());
    }

    /// A paste puts structures in as they are: no shortcut runs again.
    #[test]
    fn a_pasted_row_goes_in_at_the_caret() {
        let mut island = Island::from_notation("x");
        island.edit().insert_row(notation::parse_island("1/2"));
        assert_eq!(island.to_notation(), "x$(1/2)");
        assert!(island.cursor.is_caret());
        assert_eq!(island.cursor.index, 2);
    }

    #[test]
    fn a_paste_replaces_the_selection() {
        let mut island = Island::from_notation("ab");
        island.edit().select_row();
        island.edit().insert_row(notation::parse_island("c"));
        assert_eq!(island.to_notation(), "c");
    }

    #[test]
    fn matrix_grows_by_row_and_column() {
        let mut island = Island::new();
        island.edit().insert(super::super::ast::matrix(
            super::super::ast::MatrixKind::Grid,
            1,
            2,
        ));
        assert!(island.edit().grow_matrix(true));
        match &island.root[0] {
            Node::Matrix { cells, .. } => assert_eq!(cells.len(), 2),
            other => panic!("expected a matrix, got {other:?}"),
        }
        assert!(island.edit().grow_matrix(false));
        match &island.root[0] {
            Node::Matrix { cells, .. } => assert_eq!(cells[0].len(), 3),
            other => panic!("expected a matrix, got {other:?}"),
        }
    }
}
