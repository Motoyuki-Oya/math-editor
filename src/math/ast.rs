//! Structure of an island and the cursor that walks through it.
//!
//! An island is a [`Row`]: a flat sequence of [`Node`]s. Nodes that contain
//! sub-rows (a stack, a root, ...) expose them as numbered *slots*, so
//! navigation and editing can treat every container uniformly.

pub type Row = Vec<Node>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Delim {
    Paren,
    Bracket,
    Brace,
    Bar,
}

impl Delim {
    pub fn pair(&self) -> (char, char) {
        match self {
            Delim::Paren => ('(', ')'),
            Delim::Bracket => ('[', ']'),
            Delim::Brace => ('{', '}'),
            Delim::Bar => ('|', '|'),
        }
    }

    pub fn from_open(c: char) -> Option<Delim> {
        match c {
            '(' => Some(Delim::Paren),
            '[' => Some(Delim::Bracket),
            '{' => Some(Delim::Brace),
            '|' => Some(Delim::Bar),
            _ => None,
        }
    }
}

/// A grid `[a, b][c, d]`, on its own or behind the brace of a case split.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatrixKind {
    Grid,
    Cases,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    /// A directly typed character: a variable, a digit or an operator.
    Char(char),
    /// A named symbol such as `\alpha` or `\leq`, stored without the backslash.
    Sym(String),
    /// An upright function name such as `\sin`, stored without the backslash.
    Func(String),
    /// A rule with something above and below it; the rule can be left out.
    Stack {
        above: Row,
        below: Row,
        rule: bool,
    },
    Sqrt {
        index: Option<Row>,
        body: Row,
    },
    /// Superscript attached to whatever precedes it in the row.
    Sup(Row),
    /// Subscript attached to whatever precedes it in the row.
    Sub(Row),
    Group {
        delim: Delim,
        body: Row,
    },
    /// Any symbol with something written above and below it.
    Limits {
        sym: String,
        lower: Row,
        upper: Row,
    },
    Matrix {
        kind: MatrixKind,
        cells: Vec<Vec<Row>>,
    },
}

impl Node {
    pub fn slot_count(&self) -> usize {
        match self {
            Node::Char(_) | Node::Sym(_) | Node::Func(_) => 0,
            Node::Stack { .. } => 2,
            Node::Sqrt { index, .. } => {
                if index.is_some() {
                    2
                } else {
                    1
                }
            }
            Node::Sup(_) | Node::Sub(_) | Node::Group { .. } => 1,
            Node::Limits { .. } => 2,
            Node::Matrix { cells, .. } => cells.iter().map(|r| r.len()).sum(),
        }
    }

    pub fn slot(&self, i: usize) -> Option<&Row> {
        match self {
            Node::Char(_) | Node::Sym(_) | Node::Func(_) => None,
            Node::Stack { above, below, .. } => match i {
                0 => Some(above),
                1 => Some(below),
                _ => None,
            },
            Node::Sqrt { index, body } => match (index, i) {
                (Some(index), 0) => Some(index),
                (Some(_), 1) | (None, 0) => Some(body),
                _ => None,
            },
            Node::Sup(row) | Node::Sub(row) | Node::Group { body: row, .. } => {
                (i == 0).then_some(row)
            }
            Node::Limits { lower, upper, .. } => match i {
                0 => Some(lower),
                1 => Some(upper),
                _ => None,
            },
            Node::Matrix { cells, .. } => cells.iter().flatten().nth(i),
        }
    }

    pub fn slot_mut(&mut self, i: usize) -> Option<&mut Row> {
        match self {
            Node::Char(_) | Node::Sym(_) | Node::Func(_) => None,
            Node::Stack { above, below, .. } => match i {
                0 => Some(above),
                1 => Some(below),
                _ => None,
            },
            Node::Sqrt { index, body } => match (index.is_some(), i) {
                (true, 0) => index.as_mut(),
                (true, 1) | (false, 0) => Some(body),
                _ => None,
            },
            Node::Sup(row) | Node::Sub(row) | Node::Group { body: row, .. } => {
                (i == 0).then_some(row)
            }
            Node::Limits { lower, upper, .. } => match i {
                0 => Some(lower),
                1 => Some(upper),
                _ => None,
            },
            Node::Matrix { cells, .. } => cells.iter_mut().flatten().nth(i),
        }
    }

    /// Slot the cursor should land in when entering the node from the left.
    pub fn entry_slot(&self) -> usize {
        0
    }

    /// Slot the cursor should land in when entering the node from the right.
    pub fn exit_slot(&self) -> usize {
        match self {
            // A stack is entered from the right through its lower row.
            Node::Stack { .. } => 1,
            other => other.slot_count().saturating_sub(1),
        }
    }

    /// Matrix dimensions, if the node is a matrix.
    pub fn matrix_shape(&self) -> Option<(usize, usize)> {
        match self {
            Node::Matrix { cells, .. } => {
                Some((cells.len(), cells.first().map(|r| r.len()).unwrap_or(0)))
            }
            _ => None,
        }
    }
}

/// A position inside a formula: the chain of (node, slot) hops taken from the
/// root row, plus the offset within the row that chain leads to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cursor {
    pub path: Vec<(usize, usize)>,
    pub index: usize,
}

impl Cursor {
    pub fn root(index: usize) -> Cursor {
        Cursor {
            path: Vec::new(),
            index,
        }
    }
}

pub fn row_at<'a>(root: &'a Row, path: &[(usize, usize)]) -> Option<&'a Row> {
    let mut row = root;
    for &(node, slot) in path {
        row = row.get(node)?.slot(slot)?;
    }
    Some(row)
}

pub fn row_at_mut<'a>(root: &'a mut Row, path: &[(usize, usize)]) -> Option<&'a mut Row> {
    let mut row = root;
    for &(node, slot) in path {
        row = row.get_mut(node)?.slot_mut(slot)?;
    }
    Some(row)
}

pub fn empty_row() -> Row {
    Vec::new()
}

pub fn stack(rule: bool) -> Node {
    Node::Stack {
        above: empty_row(),
        below: empty_row(),
        rule,
    }
}

pub fn sqrt() -> Node {
    Node::Sqrt {
        index: None,
        body: empty_row(),
    }
}

pub fn nth_root() -> Node {
    Node::Sqrt {
        index: Some(empty_row()),
        body: empty_row(),
    }
}

pub fn limits(sym: &str) -> Node {
    Node::Limits {
        sym: sym.to_string(),
        lower: empty_row(),
        upper: empty_row(),
    }
}

pub fn matrix(kind: MatrixKind, rows: usize, cols: usize) -> Node {
    Node::Matrix {
        kind,
        cells: (0..rows)
            .map(|_| (0..cols).map(|_| empty_row()).collect())
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_are_addressable() {
        let node = stack(true);
        assert_eq!(node.slot_count(), 2);
        assert!(node.slot(0).is_some());
        assert!(node.slot(2).is_none());
    }

    #[test]
    fn sqrt_index_shifts_slots() {
        let plain = sqrt();
        assert_eq!(plain.slot_count(), 1);
        let nth = nth_root();
        assert_eq!(nth.slot_count(), 2);
    }

    #[test]
    fn rows_resolve_through_paths() {
        let root: Row = vec![Node::Stack {
            above: vec![Node::Char('a')],
            below: vec![Node::Char('b')],
            rule: true,
        }];
        assert_eq!(row_at(&root, &[(0, 1)]), Some(&vec![Node::Char('b')]));
        assert_eq!(row_at(&root, &[(0, 5)]), None);
    }

    #[test]
    fn matrix_slots_are_row_major() {
        let node = matrix(MatrixKind::Grid, 2, 2);
        assert_eq!(node.slot_count(), 4);
        assert_eq!(node.matrix_shape(), Some((2, 2)));
    }
}
