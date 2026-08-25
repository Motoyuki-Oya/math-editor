//! Document text. Every line is the same [`Row`] used by structural slots.
//!
//! 大きいファイルを開けるよう、行は 2 つの持ち方を持つ。構造を含む行は
//! [`Row`] そのもの、素のテキストだけの行は文字列のままで、初めて [`Row`] と
//! して見られたときに展開する（展開の結果は共有される）。行は [`Rc`] で持ち、
//! [`Text`] の複製（元に戻す履歴のスナップショット）は行を共有して、変更された
//! 行だけが書き込み時に複製される。

use std::cell::{Cell, OnceCell};
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
/// メモリを使うので、見られるまで文字列のまま保つ。まだ届いていない行は
/// [`Line::Absent`] で、範囲読みの読み込みが終わるまで空の行として見える。
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
    Absent,
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
            Line::Absent => 0,
        }
    }

    fn nodes(&self) -> &[Node] {
        match self {
            Line::Raw { source, nodes, .. } => {
                nodes.get_or_init(|| source.chars().map(Node::char).collect())
            }
            Line::Rows(row) => row,
            Line::Absent => &[],
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
        if matches!(self, Line::Absent) {
            *self = Line::Rows(Row::new());
        }
        match self {
            Line::Rows(row) => row,
            _ => unreachable!("other lines were just replaced"),
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

/// 編集で行がどう入れ替わったか: 今の文書の `from` 行からの `inserted` 行が、
/// 元の `removed` 行の代わりに立っている。文書の本体を別に持つものが、
/// 同じ入れ替えを再現するために読む。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineChange {
    pub from: usize,
    pub removed: usize,
    pub inserted: usize,
}

/// Document lines. A document always has at least one line.
#[derive(Clone, Debug, Eq)]
pub struct Text {
    lines: Vec<Rc<Line>>,
    /// まだ回収されていない行の入れ替え。互いに重ならず、今の行番号の昇順。
    changes: Vec<LineChange>,
    /// まだ届いていない行の数。残っているかを行を数えずに答えるための控え。
    absent: usize,
    /// 手元にある文字数のキャッシュ（O(1) stats用）。
    total_chars: Cell<Option<usize>>,
    /// 常駐行（Absentでない行）が存在する行番号の最小・最大範囲 [min, max)。
    resident_span: Cell<Option<(usize, usize)>>,
}

/// 行の中身が同じなら同じ文書。入れ替えの控えは持ち方の話で、意味ではない。
impl PartialEq for Text {
    fn eq(&self, other: &Text) -> bool {
        self.lines == other.lines
    }
}

impl Default for Text {
    fn default() -> Self {
        Self {
            lines: vec![Rc::new(Line::Rows(Row::new()))],
            changes: Vec::new(),
            absent: 0,
            total_chars: Cell::new(Some(0)),
            resident_span: Cell::new(Some((0, 1))),
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
        let mut total = 0;
        let line_objs: Vec<Rc<Line>> = lines
            .into_iter()
            .map(|line| {
                let (l, len) = match line {
                    SourceLine::Plain(source) => {
                        let len = source.chars().count();
                        (Line::raw(source), len)
                    }
                    SourceLine::Parsed(row) => {
                        let len = row.len();
                        (Line::Rows(row), len)
                    }
                };
                total += len;
                Rc::new(l)
            })
            .collect();
        let len = line_objs.len();
        Self {
            lines: line_objs,
            changes: Vec::new(),
            absent: 0,
            total_chars: Cell::new(Some(total)),
            resident_span: Cell::new(Some((0, len))),
        }
    }

    /// まだ届いていない行の数。残っているかを行を数えずに答える。
    pub fn absent_lines(&self) -> usize {
        self.absent
    }

    /// まだ回収されていない行の入れ替えを渡し、控えを空にする。
    /// 返る入れ替えは重ならず昇順で、`from` と `inserted` は今の文書の行番号。
    pub fn take_changes(&mut self) -> Vec<LineChange> {
        std::mem::take(&mut self.changes)
    }

    /// 文書全体を 1 つの入れ替えとして控える。読み込んだ内容を、1 行の空文書
    /// しか持っていない本体へまるごと届けるときに使う。
    pub fn mark_all_changed(&mut self) {
        self.changes = vec![LineChange {
            from: 0,
            removed: 1,
            inserted: self.lines.len(),
        }];
    }

    /// 行の入れ替えを控えへ合成する。既にある入れ替えと重なれば窓を広げて
    /// 1 つにまとめ、後ろの入れ替えは行番号を今の文書に合わせてずらす。
    /// `from` と `removed` は入れ替え前の今の文書、`inserted` は入れ替え後の行数。
    fn record(&mut self, from: usize, removed: usize, inserted: usize) {
        let mut before = Vec::new();
        let mut overlapping = Vec::new();
        let mut after = Vec::new();
        for span in std::mem::take(&mut self.changes) {
            if span.from + span.inserted <= from {
                before.push(span);
            } else if span.from >= from + removed {
                after.push(LineChange {
                    from: span.from + inserted - removed,
                    ..span
                });
            } else {
                overlapping.push(span);
            }
        }
        // 重なった入れ替えと今回の入れ替えを覆う窓。窓の中で以前の入れ替えが
        // 入れた行以外は元の文書の行なので、まとめて removed に数える
        // （触っていない行も混ざるが、同じ内容を送り直すだけで害はない）。
        let start = overlapping.first().map_or(from, |span| span.from.min(from));
        let end = overlapping.last().map_or(from + removed, |span| {
            (span.from + span.inserted).max(from + removed)
        });
        let window = end - start;
        let span_inserted: usize = overlapping.iter().map(|span| span.inserted).sum();
        let span_removed: usize = overlapping.iter().map(|span| span.removed).sum();
        let merged = LineChange {
            from: start,
            removed: span_removed + (window - span_inserted),
            inserted: window - removed + inserted,
        };
        before.push(merged);
        before.extend(after);
        self.changes = before;
    }

    /// まだ素の文字列のまま持っている行。保存形式の層がそのまま書き戻すための
    /// 早道で、編集された行は `None` になる。
    pub fn raw_line(&self, line: usize) -> Option<&str> {
        match self.lines.get(line).map(Rc::as_ref) {
            Some(Line::Raw { source, .. }) => Some(source),
            _ => None,
        }
    }

    /// 行数だけが分かっている文書。行の中身は [`Text::fill_line`] で後から届く。
    pub fn pending(line_count: usize) -> Self {
        let line_count = line_count.max(1);
        // 未着行は中身を持たないので、同じ 1 つの Rc を全行で共有する。
        let absent = Rc::new(Line::Absent);
        Self {
            lines: vec![absent; line_count],
            changes: Vec::new(),
            absent: line_count,
            total_chars: Cell::new(Some(0)),
            resident_span: Cell::new(None),
        }
    }

    /// `keep` の外の行を未着へ戻し、手元のメモリを返す。行はまた見えれば
    /// 本体から届き直す。まだ送っていない編集を含む行と `pinned`（選択や
    /// キャレットの行）は捨てない。これは編集ではないので控えには残らない。
    pub fn evict_far(&mut self, keep: std::ops::Range<usize>, pinned: &[usize]) {
        let Some((span_start, span_end)) = self.resident_span.get() else {
            return;
        };
        let absent = Rc::new(Line::Absent);
        let mut evicted_chars = 0;
        let search_start = span_start;
        let search_end = span_end.min(self.lines.len());
        let mut new_min = usize::MAX;
        let mut new_max = 0;

        for i in search_start..search_end {
            let slot = &mut self.lines[i];
            if matches!(slot.as_ref(), Line::Absent) {
                continue;
            }
            if keep.contains(&i)
                || pinned.contains(&i)
                || self
                    .changes
                    .iter()
                    .any(|c| i >= c.from && i < c.from + c.inserted)
            {
                new_min = new_min.min(i);
                new_max = new_max.max(i + 1);
                continue;
            }
            evicted_chars += slot.len();
            *slot = absent.clone();
            self.absent += 1;
        }
        self.resident_span.set(if new_min < new_max {
            Some((new_min, new_max))
        } else {
            None
        });
        if let Some(c) = self.total_chars.get() {
            self.total_chars.set(Some(c.saturating_sub(evicted_chars)));
        }
    }

    /// 走査で確定した行数へ合わせる。伸びる分は未着行として足す。
    pub fn resize_pending(&mut self, line_count: usize) {
        let target = line_count.max(1);
        if target > self.lines.len() {
            let added = target - self.lines.len();
            self.lines.resize(target, Rc::new(Line::Absent));
            self.absent += added;
        }
    }

    /// EOF基準の仮ウィンドウなど、届き直す範囲を未着へ戻す。
    pub fn forget_range(&mut self, range: std::ops::Range<usize>) {
        let absent = Rc::new(Line::Absent);
        for slot in self.lines.iter_mut().take(range.end).skip(range.start) {
            if !matches!(slot.as_ref(), Line::Absent) {
                *slot = absent.clone();
                self.absent += 1;
            }
        }
    }

    /// 文書の本体が巻き戻ったのに合わせる: `from` から先の手元の行を捨てて
    /// 届き直しを待ち、行数を合わせる。これは編集ではないので控えには残らない。
    pub fn reset_from(&mut self, from: usize, line_count: usize) {
        let line_count = line_count.max(1);
        self.lines.truncate(from.min(line_count));
        self.lines.resize(line_count, Rc::new(Line::Absent));
        self.changes.clear();
        self.absent = self
            .lines
            .iter()
            .filter(|line| matches!(line.as_ref(), Line::Absent))
            .count();
        self.total_chars.set(None);
        if let Some((min, max)) = self.resident_span.get() {
            if from <= min {
                self.resident_span.set(None);
            } else {
                self.resident_span.set(Some((min, max.min(from))));
            }
        }
    }

    /// まだ届いていない行へ中身を入れる。既に届いた行はそのまま。
    pub fn fill_line(&mut self, line: usize, source: SourceLine) {
        let Some(slot) = self.lines.get_mut(line) else {
            return;
        };
        if !matches!(slot.as_ref(), Line::Absent) {
            return;
        }
        let added = match &source {
            SourceLine::Plain(source) => source.chars().count(),
            SourceLine::Parsed(row) => row.len(),
        };
        *slot = Rc::new(match source {
            SourceLine::Plain(source) => Line::raw(source),
            SourceLine::Parsed(row) => Line::Rows(row),
        });
        self.absent -= 1;
        if let Some(c) = self.total_chars.get() {
            self.total_chars.set(Some(c + added));
        }
        let span = self.resident_span.get();
        self.resident_span.set(Some(match span {
            Some((min, max)) => (min.min(line), max.max(line + 1)),
            None => (line, line + 1),
        }));
    }

    /// `from` 以降で最初のまだ届いていない行。読み込みの続きがどこかを答える。
    pub fn first_absent(&self, from: usize) -> Option<usize> {
        (from..self.lines.len()).find(|&i| matches!(self.lines[i].as_ref(), Line::Absent))
    }

    /// この行がまだ届いていないか。行ごとに尋ねる側はこちらを使う。
    /// `first_absent` を行ごとに呼ぶと、取得済みの連なりの長さの二乗で歩く。
    pub fn is_absent(&self, line: usize) -> bool {
        self.lines
            .get(line)
            .is_some_and(|l| matches!(l.as_ref(), Line::Absent))
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn line(&self, line: usize) -> &[Node] {
        self.lines.get(line).map(|l| l.nodes()).unwrap_or(&[])
    }

    pub fn line_mut(&mut self, line: usize) -> Option<&mut Row> {
        if line >= self.lines.len() {
            return None;
        }
        self.record(line, 1, 1);
        self.count_filled(line);
        self.total_chars.set(None);
        Some(Rc::make_mut(&mut self.lines[line]).row_mut())
    }

    /// 行が編集で [`Row`] に変わるとき、まだ届いていなかった行なら数え直す。
    fn count_filled(&mut self, line: usize) {
        if matches!(self.lines[line].as_ref(), Line::Absent) {
            self.absent -= 1;
        }
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
        let removed_chars = if from.line == to.line {
            to.col.saturating_sub(from.col)
        } else {
            let mut chars = self.line_len(from.line).saturating_sub(from.col);
            for line in from.line + 1..to.line {
                chars += self.line_len(line);
            }
            chars += to.col.min(self.line_len(to.line));
            chars
        };
        // 行を丸ごと消すときは、境界の行に触らない。大きいファイルの行の
        // 出し入れが、素の行を展開せずに済むように。
        if from.col == 0 && to.col == 0 {
            self.record(from.line, to.line - from.line, 0);
            self.drain_counting(from.line, to.line);
            if let Some(c) = self.total_chars.get() {
                self.total_chars.set(Some(c.saturating_sub(removed_chars)));
            }
            if let Some((min, max)) = self.resident_span.get() {
                let delta = to.line - from.line;
                self.resident_span.set(Some((min.min(from.line), max.saturating_sub(delta))));
            }
            return from;
        }
        self.record(from.line, to.line - from.line + 1, 1);
        let tail = self.line(to.line)[to.col..].to_vec();
        self.count_filled(from.line);
        let first = Rc::make_mut(&mut self.lines[from.line]).row_mut();
        first.truncate(from.col);
        first.extend(tail);
        self.drain_counting(from.line + 1, to.line + 1);
        if let Some(c) = self.total_chars.get() {
            self.total_chars.set(Some(c.saturating_sub(removed_chars)));
        }
        if let Some((min, max)) = self.resident_span.get() {
            let delta = to.line - from.line;
            self.resident_span.set(Some((min.min(from.line), max.saturating_sub(delta))));
        }
        from
    }

    /// 行を取り除きつつ、届いていなかった行の数を合わせる。
    fn drain_counting(&mut self, from: usize, to: usize) {
        let absent = self
            .lines
            .drain(from..to)
            .filter(|line| matches!(line.as_ref(), Line::Absent))
            .count();
        self.absent -= absent;
    }

    pub fn insert(&mut self, at: Pos, mut what: Vec<Row>) -> Pos {
        let at = self.clamp(at);
        if what.is_empty() {
            return at;
        }
        let what_len = what.len();
        let added_chars: usize = what.iter().map(Row::len).sum();
        self.record(at.line, 1, what.len());
        self.count_filled(at.line);
        let first_line = Rc::make_mut(&mut self.lines[at.line]).row_mut();
        let tail: Row = first_line.split_off(at.col);
        if what.len() == 1 {
            let only = what.remove(0);
            let col = at.col + only.len();
            first_line.extend(only);
            first_line.extend(tail);
            if let Some(c) = self.total_chars.get() {
                self.total_chars.set(Some(c + added_chars));
            }
            if let Some((min, max)) = self.resident_span.get() {
                self.resident_span.set(Some((min.min(at.line), max.max(at.line + 1))));
            }
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
        if let Some(c) = self.total_chars.get() {
            self.total_chars.set(Some(c + added_chars));
        }
        if let Some((min, max)) = self.resident_span.get() {
            let shift = what_len.saturating_sub(1);
            self.resident_span.set(Some((min.min(at.line), (max + shift).max(at.line + what_len))));
        }
        end
    }

    pub fn stats(&self) -> (usize, usize) {
        let count = self.total_chars.get().unwrap_or_else(|| {
            let sum: usize = self.lines.iter().map(|line| line.len()).sum();
            self.total_chars.set(Some(sum));
            sum
        });
        (count, self.line_count())
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
    fn edits_are_collected_as_line_changes() {
        let mut text = raw_text(&["ab", "cd", "ef"]);
        text.line_mut(1);
        assert_eq!(
            text.take_changes(),
            vec![LineChange {
                from: 1,
                removed: 1,
                inserted: 1
            }]
        );
        assert_eq!(text.take_changes(), Vec::new());
    }

    #[test]
    fn overlapping_changes_merge_into_one_window() {
        let mut text = raw_text(&["a", "b", "c"]);
        // 1 行の編集に、同じ行を 3 行へ広げる挿入が重なる。
        text.line_mut(1);
        text.insert(Pos::new(1, 0), nodes_of("x\ny\nz"));
        assert_eq!(
            text.take_changes(),
            vec![LineChange {
                from: 1,
                removed: 1,
                inserted: 3
            }]
        );
    }

    #[test]
    fn a_later_change_above_shifts_the_recorded_lines_below() {
        let mut text = raw_text(&["a", "b", "c", "d"]);
        // 下の行を先に触り、その上へ行を増やす編集が続く（すべて置換の逆順など）。
        text.line_mut(2);
        text.insert(Pos::new(0, 0), nodes_of("x\ny"));
        let changes = text.take_changes();
        assert_eq!(
            changes,
            vec![
                LineChange {
                    from: 0,
                    removed: 1,
                    inserted: 2
                },
                LineChange {
                    from: 3,
                    removed: 1,
                    inserted: 1
                }
            ]
        );
    }

    #[test]
    fn whole_line_removals_are_recorded() {
        let mut text = raw_text(&["a", "b", "c"]);
        text.remove(Pos::new(0, 0), Pos::new(2, 0));
        assert_eq!(
            text.take_changes(),
            vec![LineChange {
                from: 0,
                removed: 2,
                inserted: 0
            }]
        );
    }

    #[test]
    fn eviction_keeps_the_window_pins_and_pending_edits() {
        let mut text = Text::pending(10);
        for i in 0..10 {
            text.fill_line(i, SourceLine::Plain(format!("line {i}")));
        }
        // 8 行目に未送信の編集を作る。
        text.line_mut(8);
        text.evict_far(2..5, &[6]);
        // 窓の中・ピン・編集済みは残り、それ以外は未着へ戻る。
        assert_eq!(text.raw_line(2), Some("line 2"));
        assert_eq!(text.raw_line(4), Some("line 4"));
        assert_eq!(text.raw_line(6), Some("line 6"));
        assert!(!text.is_absent(8));
        assert!(text.is_absent(0));
        assert!(text.is_absent(5));
        assert!(text.is_absent(9));
        assert_eq!(text.absent_lines(), 5);
        // 戻った行はまた届き直せる。
        text.fill_line(0, SourceLine::Plain("again".into()));
        assert_eq!(text.raw_line(0), Some("again"));
    }

    #[test]
    fn forgetting_a_provisional_window_allows_it_to_be_filled_again() {
        let mut text = Text::pending(3);
        text.fill_line(1, SourceLine::Plain("tail".into()));
        text.forget_range(1..2);
        assert!(text.is_absent(1));
        text.fill_line(1, SourceLine::Plain("remapped".into()));
        assert_eq!(text.raw_line(1), Some("remapped"));
    }

    #[test]
    fn the_absent_count_follows_fills_edits_and_removals() {
        let mut text = Text::pending(4);
        assert_eq!(text.absent_lines(), 4);
        text.fill_line(0, SourceLine::Plain("a".into()));
        text.fill_line(0, SourceLine::Plain("again".into()));
        assert_eq!(text.absent_lines(), 3);
        // 行を丸ごと消すと、届いていなかった行も数から消える。
        text.remove(Pos::new(1, 0), Pos::new(3, 0));
        assert_eq!(text.absent_lines(), 1);
        text.reset_from(0, 5);
        assert_eq!(text.absent_lines(), 5);
    }

    #[test]
    fn resetting_forgets_local_lines_and_changes() {
        let mut text = raw_text(&["a", "b", "c"]);
        text.line_mut(2);
        text.reset_from(1, 4);
        assert_eq!(text.take_changes(), Vec::new());
        assert_eq!(text.line_count(), 4);
        assert_eq!(text.raw_line(0), Some("a"));
        assert_eq!(text.first_absent(0), Some(1));
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

    #[test]
    fn stats_tracks_character_count_and_line_count_accurately() {
        let mut text = raw_text(&["hello", "world"]);
        assert_eq!(text.stats(), (10, 2));

        text.insert(Pos::new(0, 5), nodes_of("!"));
        assert_eq!(text.stats(), (11, 2));

        text.insert(Pos::new(1, 5), nodes_of("\nfoo"));
        assert_eq!(text.stats(), (14, 3));

        text.remove(Pos::new(0, 0), Pos::new(0, 1));
        assert_eq!(text.stats(), (13, 3));

        text.remove(Pos::new(1, 0), Pos::new(2, 0));
        assert_eq!(text.stats(), (8, 2));

        let mut pending = Text::pending(100);
        assert_eq!(pending.stats(), (0, 100));
        pending.fill_line(0, SourceLine::Plain("abc".into()));
        assert_eq!(pending.stats(), (3, 100));
    }

    #[test]
    fn eviction_on_large_sparse_document_only_touches_resident_range() {
        let mut text = Text::pending(1_000_000);
        assert_eq!(text.stats(), (0, 1_000_000));
        assert_eq!(text.resident_span.get(), None);

        // 500,000 行目付近だけ届く
        for i in 500_000..500_050 {
            text.fill_line(i, SourceLine::Plain(format!("line {i}")));
        }
        assert_eq!(text.resident_span.get(), Some((500_000, 500_050)));

        // 500_010..500_030 のみを keep して evict
        text.evict_far(500_010..500_030, &[]);
        assert_eq!(text.resident_span.get(), Some((500_010, 500_030)));

        assert!(text.is_absent(500_000));
        assert!(!text.is_absent(500_020));
        assert!(text.is_absent(500_040));
    }
}
