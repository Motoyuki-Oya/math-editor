//! ドキュメント: [`Item`] の行。アイランドは 1 つのアイテムとしてカウントされます。
//!
//! これは、表記と表示の両方が機能する形状であり、構造自体を保持するため、どちらも他の構造について知る必要はありません。

use super::ast::Row;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    Char(char),
    /// 列区切り文字。隣接する行のセパレータは互いに並びます。独自の内容は持ちません。
    Tab,
    /// アイランド: 2 次元を必要とするテキストの一部で、表記法としてではなく構造自体として保持されます。
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

/// 2 つの項目の間の場所。 `col` はバイトではなく項目をカウントします。
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

/// キャレット (`anchor == head`) または `anchor` から伸びる選択範囲。
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

/// ドキュメントの行。常に少なくとも 1 つの行があります。
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

    /// 2 つの場所の間のすべてを削除し、それらが結合した場所を返します。
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

    /// 項目の行を挿入し、その直後の場所を返します。
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

    /// その場で編集される「at」の島。アイランドの編集はドキュメントの編集です。書き戻すドキュメントのコピーはありません。
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

    /// ステータス バーの文字と行。数式は 1 つとしてカウントされます。
    pub fn stats(&self) -> (usize, usize) {
        (self.lines.iter().map(Vec::len).sum(), self.line_count())
    }
}

/// 項目がある場合は、同じ行の 1 つの項目を左に配置します。
pub fn before_col(at: Pos) -> Option<Pos> {
    (at.col > 0).then(|| Pos::new(at.line, at.col - 1))
}

/// 項目を 1 つ左に配置します。項目を挿入してポイントした後に使用されます。
pub fn before_pos(at: Pos) -> Pos {
    before_col(at).unwrap_or(at)
}

pub fn items_of(text: &str) -> Vec<Vec<Item>> {
    text.split('\n')
        .map(|line| line.chars().map(Item::Char).collect())
        .collect()
}
