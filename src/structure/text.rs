//! A document: lines of [`Item`]s, where an island counts as one item.
//!
//! This is the shape both the notation and the display work from, and it holds
//! the structures themselves, so neither of them has to know about the other.

use super::ast::Row;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    Char(char),
    /// A column separator. Separators on neighbouring lines line up with each
    /// other; it carries no content of its own.
    Tab,
    /// An island: a piece of the text that needs two dimensions, held as the
    /// structure itself rather than as the notation it is stored in.
    Math(Row),
}

impl Item {
    pub fn as_char(&self) -> Option<char> {
        match self {
            Item::Char(c) => Some(*c),
            Item::Tab | Item::Math(_) => None,
        }
    }
}

/// A place between two items. `col` counts items, not bytes.
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

/// A caret (`anchor == head`) or a selected range, growing from `anchor`.
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

/// The lines of the document. There is always at least one line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Text {
    lines: Vec<Vec<Item>>,
}

impl Default for Text {
    fn default() -> Self {
        Self {
            lines: vec![Vec::new()],
        }
    }
}

impl Text {
    pub fn from_lines(lines: Vec<Vec<Item>>) -> Self {
        Self {
            lines: if lines.is_empty() {
                vec![Vec::new()]
            } else {
                lines
            },
        }
    }

    pub fn lines(&self) -> &[Vec<Item>] {
        &self.lines
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn line(&self, line: usize) -> &[Item] {
        self.lines.get(line).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn line_len(&self, line: usize) -> usize {
        self.line(line).len()
    }

    pub fn item_at(&self, at: Pos) -> Option<&Item> {
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

    pub fn slice(&self, from: Pos, to: Pos) -> Vec<Vec<Item>> {
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

    /// Removes everything between the two places and returns where they joined.
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

    /// Inserts lines of items and returns the place just after them.
    pub fn insert(&mut self, at: Pos, mut what: Vec<Vec<Item>>) -> Pos {
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
        let mut rest: Vec<Vec<Item>> = what;
        let mut last_line = last;
        last_line.extend(tail);
        rest.push(last_line);
        for (offset, line) in rest.into_iter().enumerate() {
            self.lines.insert(at.line + 1 + offset, line);
        }
        end
    }

    /// The island at `at`, to be edited in place. Editing an island is editing
    /// the document: there is no copy of it to write back.
    pub fn math_at_mut(&mut self, at: Pos) -> Option<&mut Row> {
        match self
            .lines
            .get_mut(at.line)
            .and_then(|line| line.get_mut(at.col))
        {
            Some(Item::Math(row)) => Some(row),
            _ => None,
        }
    }

    /// Characters and lines, for the status bar. A formula counts as one.
    pub fn stats(&self) -> (usize, usize) {
        (self.lines.iter().map(Vec::len).sum(), self.line_count())
    }
}

/// The place one item to the left on the same line, if there is one.
pub fn before_col(at: Pos) -> Option<Pos> {
    (at.col > 0).then(|| Pos::new(at.line, at.col - 1))
}

/// The place one item to the left, used after inserting an item to point at it.
pub fn before_pos(at: Pos) -> Pos {
    before_col(at).unwrap_or(at)
}

pub fn items_of(text: &str) -> Vec<Vec<Item>> {
    text.split('\n')
        .map(|line| line.chars().map(Item::Char).collect())
        .collect()
}
