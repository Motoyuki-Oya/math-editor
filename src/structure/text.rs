//! Document text. Every line is the same [`Row`] used by structural slots.
//!
//! 大きいファイルを開けるよう、行は 2 つの持ち方を持つ。構造を含む行は
//! [`Row`] そのもの、素のテキストだけの行は文字列のままで、初めて [`Row`] と
//! して見られたときに展開する（展開の結果は共有される）。行は [`Rc`] で持ち、
//! [`Text`] の複製（元に戻す履歴のスナップショット）は行を共有して、変更された
//! 行だけが書き込み時に複製される。

use std::cell::OnceCell;
use std::rc::Rc;

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

/// ファイルから来た行の姿。呼び出し元（保存形式の層）が、素のテキストだけの
/// 行と、構造を含むため解析済みの行を区別して渡す。
pub enum SourceLine {
    Plain(String),
    Parsed(Row),
}

/// 1 行。素のテキストの行は、1 文字を 1 [`Node`] に展開すると元の何十倍もの
/// メモリを使うので、見られるまで文字列のまま保つ。
#[derive(Clone, Debug)]
enum Line {
    Raw {
        source: String,
        /// 文字数。`line_len` が展開なしで答えられるように、数えた結果を覚える。
        count: OnceCell<usize>,
        /// 展開した [`Row`]。行を [`Row`] として見るものすべてで共有される。
        nodes: OnceCell<Row>,
    },
    Rows(Row),
}

impl Line {
    fn raw(source: String) -> Line {
        Line::Raw {
            source,
            count: OnceCell::new(),
            nodes: OnceCell::new(),
        }
    }

    fn len(&self) -> usize {
        match self {
            Line::Raw { source, count, .. } => *count.get_or_init(|| source.chars().count()),
            Line::Rows(row) => row.len(),
        }
    }

    fn nodes(&self) -> &[Node] {
        match self {
            Line::Raw { source, nodes, .. } => {
                nodes.get_or_init(|| source.chars().map(Node::char).collect())
            }
            Line::Rows(row) => row,
        }
    }

    /// 編集は [`Row`] に対して行う。素の行はここで展開され、文字列は手放す。
    fn row_mut(&mut self) -> &mut Row {
        if let Line::Raw { source, nodes, .. } = self {
            let row = nodes
                .take()
                .unwrap_or_else(|| source.chars().map(Node::char).collect());
            *self = Line::Rows(row);
        }
        match self {
            Line::Rows(row) => row,
            Line::Raw { .. } => unreachable!("raw lines were just replaced"),
        }
    }
}

/// 行の中身は同じ文字の列なら等しい。素のままか展開済みかは持ち方の違いで、
/// 意味の違いではない。
impl PartialEq for Line {
    fn eq(&self, other: &Line) -> bool {
        self.nodes() == other.nodes()
    }
}

impl Eq for Line {}

/// Document lines. A document always has at least one line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Text {
    lines: Vec<Rc<Line>>,
}

impl Default for Text {
    fn default() -> Self {
        Self {
            lines: vec![Rc::new(Line::Rows(Row::new()))],
        }
    }
}

impl Text {
    /// テスト用: 解析済みの行から文書を作る。製品コードは [`Text::compose`] を通る。
    #[cfg(test)]
    pub fn from_lines(lines: Vec<Row>) -> Self {
        Self::compose(lines.into_iter().map(SourceLine::Parsed).collect())
    }

    /// 保存形式の層が読み取った行から文書を作る。素のテキストだけの行は
    /// 文字列のまま持ち、書き戻しもそのまま行われる。
    pub fn compose(lines: Vec<SourceLine>) -> Self {
        if lines.is_empty() {
            return Self::default();
        }
        Self {
            lines: lines
                .into_iter()
                .map(|line| {
                    Rc::new(match line {
                        SourceLine::Plain(source) => Line::raw(source),
                        SourceLine::Parsed(row) => Line::Rows(row),
                    })
                })
                .collect(),
        }
    }

    /// まだ素の文字列のまま持っている行。保存形式の層がそのまま書き戻すための
    /// 早道で、編集された行は `None` になる。
    pub fn raw_line(&self, line: usize) -> Option<&str> {
        match self.lines.get(line).map(Rc::as_ref) {
            Some(Line::Raw { source, .. }) => Some(source),
            _ => None,
        }
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn line(&self, line: usize) -> &[Node] {
        self.lines.get(line).map(|l| l.nodes()).unwrap_or(&[])
    }

    pub fn line_mut(&mut self, line: usize) -> Option<&mut Row> {
        Some(Rc::make_mut(self.lines.get_mut(line)?).row_mut())
    }

    pub fn line_len(&self, line: usize) -> usize {
        self.lines.get(line).map(|l| l.len()).unwrap_or(0)
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
        // 行を丸ごと消すときは、境界の行に触らない。大きいファイルの行の
        // 出し入れが、素の行を展開せずに済むように。
        if from.col == 0 && to.col == 0 {
            self.lines.drain(from.line..to.line);
            return from;
        }
        let tail = self.line(to.line)[to.col..].to_vec();
        let first = Rc::make_mut(&mut self.lines[from.line]).row_mut();
        first.truncate(from.col);
        first.extend(tail);
        self.lines.drain(from.line + 1..=to.line);
        from
    }

    pub fn insert(&mut self, at: Pos, mut what: Vec<Row>) -> Pos {
        let at = self.clamp(at);
        if what.is_empty() {
            return at;
        }
        let first_line = Rc::make_mut(&mut self.lines[at.line]).row_mut();
        let tail: Row = first_line.split_off(at.col);
        if what.len() == 1 {
            let only = what.remove(0);
            let col = at.col + only.len();
            first_line.extend(only);
            first_line.extend(tail);
            return Pos::new(at.line, col);
        }
        let last = what.pop().expect("more than one line");
        let first = what.remove(0);
        first_line.extend(first);
        let end = Pos::new(at.line + what.len() + 1, last.len());
        let mut rest = what;
        let mut last_line = last;
        last_line.extend(tail);
        rest.push(last_line);
        for (offset, line) in rest.into_iter().enumerate() {
            self.lines
                .insert(at.line + 1 + offset, Rc::new(Line::Rows(line)));
        }
        end
    }

    pub fn stats(&self) -> (usize, usize) {
        (
            self.lines.iter().map(|line| line.len()).sum(),
            self.line_count(),
        )
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

#[cfg(test)]
mod tests {
    use super::*;

    fn raw_text(lines: &[&str]) -> Text {
        Text::compose(
            lines
                .iter()
                .map(|line| SourceLine::Plain(line.to_string()))
                .collect(),
        )
    }

    #[test]
    fn a_raw_line_reads_as_ordinary_characters() {
        let text = raw_text(&["ab", "cd"]);
        assert_eq!(text.line_len(0), 2);
        assert_eq!(text.line(1), &[Node::char('c'), Node::char('d')][..]);
        assert_eq!(text.stats(), (4, 2));
    }

    #[test]
    fn raw_and_parsed_lines_with_the_same_characters_are_equal() {
        let raw = raw_text(&["ab"]);
        let parsed = Text::from_lines(nodes_of("ab"));
        assert_eq!(raw, parsed);
    }

    #[test]
    fn editing_a_line_keeps_the_others_raw() {
        let mut text = raw_text(&["ab", "cd", "ef"]);
        text.insert(Pos::new(1, 1), vec![vec![Node::char('X')]]);
        assert_eq!(text.raw_line(0), Some("ab"));
        assert_eq!(text.raw_line(1), None);
        assert_eq!(text.raw_line(2), Some("ef"));
        assert_eq!(text.line(1).len(), 3);
    }

    #[test]
    fn viewing_a_raw_line_does_not_drop_its_source() {
        let text = raw_text(&["ab"]);
        assert_eq!(text.line(0).len(), 2);
        assert_eq!(text.raw_line(0), Some("ab"));
    }

    #[test]
    fn removing_whole_lines_leaves_the_rest_raw() {
        let mut text = raw_text(&["ab", "cd", "ef"]);
        let at = text.remove(Pos::new(0, 0), Pos::new(2, 0));
        assert_eq!(at, Pos::new(0, 0));
        assert_eq!(text.line_count(), 1);
        assert_eq!(text.raw_line(0), Some("ef"));
    }

    #[test]
    fn a_snapshot_shares_lines_until_they_are_edited() {
        let mut text = raw_text(&["ab", "cd"]);
        let snapshot = text.clone();
        text.insert(Pos::new(0, 0), vec![vec![Node::char('X')]]);
        assert_eq!(text.line(0).len(), 3);
        assert_eq!(snapshot.line(0).len(), 2);
        assert_eq!(snapshot.raw_line(0), Some("ab"));
    }

    #[test]
    fn removing_within_lines_joins_the_boundaries() {
        let mut text = raw_text(&["abc", "def", "ghi"]);
        let at = text.remove(Pos::new(0, 2), Pos::new(2, 1));
        assert_eq!(at, Pos::new(0, 2));
        assert_eq!(text.line_count(), 1);
        assert_eq!(
            text.line(0),
            &[
                Node::char('a'),
                Node::char('b'),
                Node::char('h'),
                Node::char('i')
            ][..]
        );
    }

    #[test]
    fn inserting_lines_splits_a_line_and_reports_the_end() {
        let mut text = raw_text(&["abcd"]);
        let end = text.insert(Pos::new(0, 2), nodes_of("X\nY"));
        assert_eq!(end, Pos::new(1, 1));
        assert_eq!(text.line_count(), 2);
        assert_eq!(text.line(0), nodes_of("abX")[0].as_slice());
        assert_eq!(text.line(1), nodes_of("Ycd")[0].as_slice());
    }
}
