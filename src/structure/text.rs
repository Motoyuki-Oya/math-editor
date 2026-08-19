//! Document text. Every line is the same [`Row`] used by structural slots.

use super::ast::{Node, NodeKind, Row};

/// A position between top-level nodes. `col` counts nodes, not bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

impl Pos {
    pub fn new(line: usize, col: usize) -> Self {
        Self { line, col }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sel {
    pub anchor: Pos,
    pub head: Pos,
}

impl Sel {
    pub fn caret(at: Pos) -> Self {
        Self {
            anchor: at,
            head: at,
        }
    }

    pub fn range(from: Pos, to: Pos) -> Self {
        Self {
            anchor: from,
            head: to,
        }
    }

    pub fn start(&self) -> Pos {
        self.anchor.min(self.head)
    }

    pub fn end(&self) -> Pos {
        self.anchor.max(self.head)
    }

    pub fn is_caret(&self) -> bool {
        self.anchor == self.head
    }
}

/// Document lines. A document always has at least one line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Text {
    lines: Vec<Row>,
}

impl Default for Text {
    fn default() -> Self {
        Self {
            lines: vec![Row::new()],
        }
    }
}

impl Text {
    pub fn from_lines(lines: Vec<Row>) -> Self {
        Self {
            lines: if lines.is_empty() {
                vec![Row::new()]
            } else {
                lines
            },
        }
    }

    pub fn lines(&self) -> &[Row] {
        &self.lines
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn line(&self, line: usize) -> &[Node] {
        self.lines.get(line).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn line_mut(&mut self, line: usize) -> Option<&mut Row> {
        self.lines.get_mut(line)
    }

    pub fn line_len(&self, line: usize) -> usize {
        self.line(line).len()
    }

    pub fn node_at(&self, at: Pos) -> Option<&Node> {
        self.line(at.line).get(at.col)
    }

    pub fn end(&self) -> Pos {
        let line = self.line_count() - 1;
        Pos::new(line, self.line_len(line))
    }

    pub fn clamp(&self, at: Pos) -> Pos {
        let line = at.line.min(self.line_count() - 1);
        Pos::new(line, at.col.min(self.line_len(line)))
    }

    pub fn slice(&self, from: Pos, to: Pos) -> Vec<Row> {
        let (from, to) = (self.clamp(from), self.clamp(to));
        if from.line == to.line {
            return vec![self.line(from.line)[from.col..to.col].to_vec()];
        }
        let mut out = vec![self.line(from.line)[from.col..].to_vec()];
        for line in from.line + 1..to.line {
            out.push(self.line(line).to_vec());
        }
        out.push(self.line(to.line)[..to.col].to_vec());
        out
    }

    pub fn remove(&mut self, from: Pos, to: Pos) -> Pos {
        let (from, to) = (self.clamp(from), self.clamp(to));
        if from == to {
            return from;
        }
        let tail = self.lines[to.line][to.col..].to_vec();
        self.lines[from.line].truncate(from.col);
        self.lines[from.line].extend(tail);
        self.lines.drain(from.line + 1..=to.line);
        from
    }

    pub fn insert(&mut self, at: Pos, mut what: Vec<Row>) -> Pos {
        let at = self.clamp(at);
        if what.is_empty() {
            return at;
        }
        let tail = self.lines[at.line][at.col..].to_vec();
        self.lines[at.line].truncate(at.col);
        if what.len() == 1 {
            let only = what.remove(0);
            let col = at.col + only.len();
            self.lines[at.line].extend(only);
            self.lines[at.line].extend(tail);
            return Pos::new(at.line, col);
        }
        let last = what.pop().expect("more than one line");
        let first = what.remove(0);
        self.lines[at.line].extend(first);
        let end = Pos::new(at.line + what.len() + 1, last.len());
        let mut rest = what;
        let mut last_line = last;
        last_line.extend(tail);
        rest.push(last_line);
        for (offset, line) in rest.into_iter().enumerate() {
            self.lines.insert(at.line + 1 + offset, line);
        }
        end
    }

    pub fn stats(&self) -> (usize, usize) {
        (self.lines.iter().map(Vec::len).sum(), self.line_count())
    }
}

pub fn before_col(at: Pos) -> Option<Pos> {
    (at.col > 0).then(|| Pos::new(at.line, at.col - 1))
}

pub fn nodes_of(text: &str) -> Vec<Row> {
    text.split('\n')
        .map(|line| line.chars().map(Node::char).collect())
        .collect()
}

pub fn as_char(node: &Node) -> Option<char> {
    match node.kind {
        NodeKind::Char(c) => Some(c),
        _ => None,
    }
}
