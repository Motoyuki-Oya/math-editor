//! Document text. Every line is the same [`Row`] used by structural slots.
//!
//! 大きいファイルを開けるよう、行は 2 つの持ち方を持つ。構造を含む行は
//! [`Row`] そのもの、素のテキストだけの行は文字列のままで、初めて [`Row`] と
//! して見られたときに展開する（展開の結果は共有される）。行は [`Rc`] で持ち、
//! [`Text`] の複製（元に戻す履歴のスナップショット）は行を共有して、変更された
//! 行だけが書き込み時に複製される。

use std::cell::{Cell, OnceCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use super::ast::{Node, NodeKind, Row};

pub const PAGE_SHIFT: usize = 10; // 1024 = 2^10
pub const PAGE_SIZE: usize = 1 << PAGE_SHIFT;
pub const PAGE_MASK: usize = PAGE_SIZE - 1;

type Page = Box<[Rc<Line>; PAGE_SIZE]>;

fn new_absent_page() -> Page {
    let absent = Rc::new(Line::Absent);
    Box::new(std::array::from_fn(|_| absent.clone()))
}

/// 行番号から (ページ番号, ページ内オフセット) をビット演算で瞬時に分解するマクロ
macro_rules! page_and_offset {
    ($line:expr) => {
        (($line) >> PAGE_SHIFT, ($line) & PAGE_MASK)
    };
}

/// ページが存在すればその行参照を、存在しなければ Absent を渡して処理するマクロ
macro_rules! with_line {
    ($self:expr, $line:expr, |$l:ident| $body:block) => {{
        let (page_idx, offset) = page_and_offset!($line);
        if let Some(page) = $self.pages.get(&page_idx) {
            let $l = page[offset].as_ref();
            $body
        } else {
            let $l = &Line::Absent;
            $body
        }
    }};
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CharKind {
    Alphanumeric, // [a-zA-Z0-9_] + 全角英数字
    Kanji,
    Hiragana,
    Katakana,
    Whitespace,
    Punctuation,
}

pub fn char_kind(c: char) -> CharKind {
    if c.is_whitespace() {
        CharKind::Whitespace
    } else if c.is_ascii_alphanumeric()
        || c == '_'
        || ('\u{FF10}'..='\u{FF19}').contains(&c)
        || ('\u{FF21}'..='\u{FF3A}').contains(&c)
        || ('\u{FF41}'..='\u{FF5A}').contains(&c)
        || c == '＿'
    {
        CharKind::Alphanumeric
    } else if ('\u{4E00}'..='\u{9FFF}').contains(&c)
        || ('\u{3400}'..='\u{4DBF}').contains(&c)
        || ('\u{F900}'..='\u{FAFF}').contains(&c)
    {
        CharKind::Kanji
    } else if ('\u{3040}'..='\u{309F}').contains(&c) {
        CharKind::Hiragana
    } else if ('\u{30A0}'..='\u{30FF}').contains(&c)
        || ('\u{31F0}'..='\u{31FF}').contains(&c)
        || ('\u{FF65}'..='\u{FF9F}').contains(&c)
        || c == 'ー'
        || c == '・'
    {
        CharKind::Katakana
    } else {
        CharKind::Punctuation
    }
}

pub fn is_word(c: char) -> bool {
    char_kind(c) != CharKind::Whitespace && char_kind(c) != CharKind::Punctuation
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
    line_count: usize,
    pages: BTreeMap<usize, Page>,
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
        if self.line_count != other.line_count {
            return false;
        }
        for i in 0..self.line_count {
            if self.line(i) != other.line(i) {
                return false;
            }
        }
        true
    }
}

impl Default for Text {
    fn default() -> Self {
        let mut pages = BTreeMap::new();
        let mut page = new_absent_page();
        page[0] = Rc::new(Line::Rows(Row::new()));
        pages.insert(0, page);
        Self {
            line_count: 1,
            pages,
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
        let line_count = lines.len();
        let mut total = 0;
        let mut pages = BTreeMap::new();
        for (chunk_idx, chunk) in lines.chunks(PAGE_SIZE).enumerate() {
            let mut page = new_absent_page();
            for (offset, line) in chunk.iter().enumerate() {
                let (l, len) = match line {
                    SourceLine::Plain(source) => {
                        let clean = source.trim_end_matches(['\r', '\n']).to_string();
                        let len = clean.chars().count();
                        (Line::raw(clean), len)
                    }
                    SourceLine::Parsed(row) => {
                        let len = row.len();
                        (Line::Rows(row.clone()), len)
                    }
                };
                total += len;
                page[offset] = Rc::new(l);
            }
            pages.insert(chunk_idx, page);
        }
        Self {
            line_count,
            pages,
            changes: Vec::new(),
            absent: 0,
            total_chars: Cell::new(Some(total)),
            resident_span: Cell::new(Some((0, line_count))),
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
    #[allow(dead_code)]
    pub fn mark_all_changed(&mut self) {
        self.changes = vec![LineChange {
            from: 0,
            removed: 1,
            inserted: self.line_count,
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
        if line >= self.line_count {
            return None;
        }
        with_line!(self, line, |l| {
            match l {
                Line::Raw { source, .. } => Some(source.as_str()),
                _ => None,
            }
        })
    }

    /// 行数だけが分かっている文書。行の中身は [`Text::fill_line`] で後から届く。
    pub fn pending(line_count: usize) -> Self {
        let line_count = line_count.max(1);
        Self {
            line_count,
            pages: BTreeMap::new(),
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
        let search_end = span_end.min(self.line_count);
        let mut new_min = usize::MAX;
        let mut new_max = 0;

        let start_page = search_start >> PAGE_SHIFT;
        let end_page = if search_end == 0 {
            0
        } else {
            (search_end - 1) >> PAGE_SHIFT
        };

        let mut empty_pages = Vec::new();

        for page_idx in start_page..=end_page {
            let Some(page) = self.pages.get_mut(&page_idx) else {
                continue;
            };
            let page_base = page_idx << PAGE_SHIFT;
            let mut page_has_resident = false;

            for offset in 0..PAGE_SIZE {
                let line = page_base + offset;
                if line >= self.line_count {
                    break;
                }
                let slot = &mut page[offset];
                if matches!(slot.as_ref(), Line::Absent) {
                    continue;
                }
                if keep.contains(&line)
                    || pinned.contains(&line)
                    || self
                        .changes
                        .iter()
                        .any(|c| line >= c.from && line < c.from + c.inserted)
                {
                    new_min = new_min.min(line);
                    new_max = new_max.max(line + 1);
                    page_has_resident = true;
                    continue;
                }
                evicted_chars += slot.len();
                *slot = absent.clone();
                self.absent += 1;
            }

            if !page_has_resident {
                empty_pages.push(page_idx);
            }
        }

        for page_idx in empty_pages {
            self.pages.remove(&page_idx);
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
        if target > self.line_count {
            let added = target - self.line_count;
            self.line_count = target;
            self.absent += added;
        }
    }

    /// EOF基準の仮ウィンドウなど、届き直す範囲を未着へ戻す。
    pub fn forget_range(&mut self, range: std::ops::Range<usize>) {
        let absent = Rc::new(Line::Absent);
        let start = range.start.min(self.line_count);
        let end = range.end.min(self.line_count);
        if start >= end {
            return;
        }
        let start_page = start >> PAGE_SHIFT;
        let end_page = (end - 1) >> PAGE_SHIFT;

        for page_idx in start_page..=end_page {
            if let Some(page) = self.pages.get_mut(&page_idx) {
                let page_base = page_idx << PAGE_SHIFT;
                for offset in 0..PAGE_SIZE {
                    let line = page_base + offset;
                    if line >= start && line < end {
                        let slot = &mut page[offset];
                        if !matches!(slot.as_ref(), Line::Absent) {
                            *slot = absent.clone();
                            self.absent += 1;
                        }
                    }
                }
            }
        }
    }

    /// 文書の本体が巻き戻ったのに合わせる: `from` から先の手元の行を捨てて
    /// 届き直しを待ち、行数を合わせる。これは編集ではないので控えには残らない。
    pub fn reset_from(&mut self, from: usize, line_count: usize) {
        let line_count = line_count.max(1);
        let from_page = from >> PAGE_SHIFT;
        let keys_to_remove: Vec<usize> = self
            .pages
            .keys()
            .copied()
            .filter(|&p| p > from_page)
            .collect();
        for k in keys_to_remove {
            self.pages.remove(&k);
        }
        if let Some(page) = self.pages.get_mut(&from_page) {
            let from_offset = from & PAGE_MASK;
            let absent = Rc::new(Line::Absent);
            for offset in from_offset..PAGE_SIZE {
                page[offset] = absent.clone();
            }
        }
        self.line_count = line_count;
        self.changes.clear();
        let mut resident_count = 0;
        for (&page_idx, page) in &self.pages {
            let page_base = page_idx << PAGE_SHIFT;
            for offset in 0..PAGE_SIZE {
                let line = page_base + offset;
                if line < self.line_count && !matches!(page[offset].as_ref(), Line::Absent) {
                    resident_count += 1;
                }
            }
        }
        self.absent = self.line_count.saturating_sub(resident_count);
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
        if line >= self.line_count {
            return;
        }
        let (page_idx, offset) = page_and_offset!(line);
        let page = self.pages.entry(page_idx).or_insert_with(new_absent_page);
        if !matches!(page[offset].as_ref(), Line::Absent) {
            return;
        }
        let (l, added) = match source {
            SourceLine::Plain(source) => {
                let clean = source.trim_end_matches(['\r', '\n']).to_string();
                let len = clean.chars().count();
                (Line::raw(clean), len)
            }
            SourceLine::Parsed(row) => {
                let len = row.len();
                (Line::Rows(row), len)
            }
        };
        page[offset] = Rc::new(l);
        self.absent = self.absent.saturating_sub(1);
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
        (from..self.line_count).find(|&line| self.is_absent(line))
    }

    /// この行がまだ届いていないか。行ごとに尋ねる側はこちらを使う。
    /// `first_absent` を行ごとに呼ぶと、取得済みの連なりの長さの二乗で歩く。
    pub fn is_absent(&self, line: usize) -> bool {
        if line >= self.line_count {
            return false;
        }
        with_line!(self, line, |l| { matches!(l, Line::Absent) })
    }

    /// 手元に存在する（未着でない）連続行範囲のリストを返す。
    /// スパースな巨大文書で、未着行を何百万行も無駄に走査するのを防ぐ。
    pub fn resident_line_ranges(&self) -> Vec<std::ops::Range<usize>> {
        let mut ranges = Vec::new();
        for (&page_idx, page) in &self.pages {
            let page_base = page_idx << PAGE_SHIFT;
            let mut range_start: Option<usize> = None;
            for offset in 0..PAGE_SIZE {
                let line = page_base + offset;
                if line >= self.line_count {
                    break;
                }
                let is_resident = !matches!(page[offset].as_ref(), Line::Absent);
                if is_resident {
                    if range_start.is_none() {
                        range_start = Some(line);
                    }
                } else if let Some(start) = range_start.take() {
                    ranges.push(start..line);
                }
            }
            if let Some(start) = range_start {
                ranges.push(start..(page_base + PAGE_SIZE).min(self.line_count));
            }
        }
        ranges
    }

    pub fn line_count(&self) -> usize {
        self.line_count
    }

    pub fn line(&self, line: usize) -> &[Node] {
        if line >= self.line_count {
            return &[];
        }
        with_line!(self, line, |l| { l.nodes() })
    }

    /// 行が素のテキスト（未展開）の場合に、展開せずに直接文字列スライスを返す。
    /// 検索などで展開コスト・アロケーションをゼロにする。
    pub fn line_str(&self, line: usize) -> Option<&str> {
        if line >= self.line_count {
            return None;
        }
        with_line!(self, line, |l| {
            match l {
                Line::Raw { source, .. } => Some(source.as_str()),
                _ => None,
            }
        })
    }

    pub fn line_mut(&mut self, line: usize) -> Option<&mut Row> {
        if line >= self.line_count {
            return None;
        }
        self.record(line, 1, 1);
        self.count_filled(line);
        self.total_chars.set(None);
        let (page_idx, offset) = page_and_offset!(line);
        let page = self.pages.entry(page_idx).or_insert_with(new_absent_page);
        Some(Rc::make_mut(&mut page[offset]).row_mut())
    }

    /// 行が編集で [`Row`] に変わるとき、まだ届いていなかった行なら数え直す。
    fn count_filled(&mut self, line: usize) {
        if self.is_absent(line) {
            self.absent = self.absent.saturating_sub(1);
        }
    }

    pub fn line_len(&self, line: usize) -> usize {
        if line >= self.line_count {
            return 0;
        }
        with_line!(self, line, |l| { l.len() })
    }

    pub fn node_at(&self, at: Pos) -> Option<&Node> {
        self.line(at.line).get(at.col)
    }

    pub fn end(&self) -> Pos {
        let line = self.line_count.saturating_sub(1);
        Pos::new(line, self.line_len(line))
    }

    pub fn clamp(&self, at: Pos) -> Pos {
        let line = at.line.min(self.line_count.saturating_sub(1));
        if self.is_absent(line) {
            return Pos::new(line, at.col);
        }
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

    fn get_line_rc(&self, line: usize) -> Rc<Line> {
        let (page_idx, offset) = page_and_offset!(line);
        self.pages
            .get(&page_idx)
            .map(|p| p[offset].clone())
            .unwrap_or_else(|| Rc::new(Line::Absent))
    }

    fn set_line_rc(&mut self, line: usize, rc: Rc<Line>) {
        let (page_idx, offset) = page_and_offset!(line);
        let page = self.pages.entry(page_idx).or_insert_with(new_absent_page);
        page[offset] = rc;
    }

    fn insert_lines_at(&mut self, at: usize, lines: Vec<Rc<Line>>) {
        if lines.is_empty() {
            return;
        }
        let count = lines.len();
        for i in (at..self.line_count).rev() {
            let prev = self.get_line_rc(i);
            self.set_line_rc(i + count, prev);
        }
        for (offset, line) in lines.into_iter().enumerate() {
            self.set_line_rc(at + offset, line);
        }
        self.line_count += count;
    }

    fn remove_lines_range(&mut self, from: usize, to: usize) {
        if from >= to || from >= self.line_count {
            return;
        }
        let to = to.min(self.line_count);
        let count = to - from;
        let mut absent_removed = 0;
        for i in from..to {
            if self.is_absent(i) {
                absent_removed += 1;
            }
        }
        self.absent = self.absent.saturating_sub(absent_removed);

        for i in from..self.line_count.saturating_sub(count) {
            let next = self.get_line_rc(i + count);
            self.set_line_rc(i, next);
        }
        let absent = Rc::new(Line::Absent);
        for i in self.line_count.saturating_sub(count)..self.line_count {
            self.set_line_rc(i, absent.clone());
        }
        self.line_count = self.line_count.saturating_sub(count);
        // 使われなくなった末尾ページをクリーンアップ
        let last_page = if self.line_count == 0 {
            0
        } else {
            (self.line_count - 1) >> PAGE_SHIFT
        };
        let keys_to_remove: Vec<usize> = self
            .pages
            .keys()
            .copied()
            .filter(|&p| p > last_page)
            .collect();
        for k in keys_to_remove {
            self.pages.remove(&k);
        }
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
        if from.col == 0 && to.col == 0 {
            self.record(from.line, to.line - from.line, 0);
            self.remove_lines_range(from.line, to.line);
            if let Some(c) = self.total_chars.get() {
                self.total_chars.set(Some(c.saturating_sub(removed_chars)));
            }
            if let Some((min, max)) = self.resident_span.get() {
                let delta = to.line - from.line;
                self.resident_span
                    .set(Some((min.min(from.line), max.saturating_sub(delta))));
            }
            return from;
        }
        self.record(from.line, to.line - from.line + 1, 1);
        let tail = self.line(to.line)[to.col..].to_vec();
        self.count_filled(from.line);
        let (page_idx, offset) = page_and_offset!(from.line);
        let page = self.pages.entry(page_idx).or_insert_with(new_absent_page);
        let first = Rc::make_mut(&mut page[offset]).row_mut();
        first.truncate(from.col);
        first.extend(tail);
        self.remove_lines_range(from.line + 1, to.line + 1);
        if let Some(c) = self.total_chars.get() {
            self.total_chars.set(Some(c.saturating_sub(removed_chars)));
        }
        if let Some((min, max)) = self.resident_span.get() {
            let delta = to.line - from.line;
            self.resident_span
                .set(Some((min.min(from.line), max.saturating_sub(delta))));
        }
        from
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
        let (page_idx, offset) = page_and_offset!(at.line);
        let page = self.pages.entry(page_idx).or_insert_with(new_absent_page);
        let first_line = Rc::make_mut(&mut page[offset]).row_mut();
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
                self.resident_span
                    .set(Some((min.min(at.line), max.max(at.line + 1))));
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
        let new_lines: Vec<Rc<Line>> = rest.into_iter().map(|r| Rc::new(Line::Rows(r))).collect();
        self.insert_lines_at(at.line + 1, new_lines);
        if let Some(c) = self.total_chars.get() {
            self.total_chars.set(Some(c + added_chars));
        }
        if let Some((min, max)) = self.resident_span.get() {
            let shift = what_len.saturating_sub(1);
            self.resident_span.set(Some((
                min.min(at.line),
                (max + shift).max(at.line + what_len),
            )));
        }
        end
    }

    pub fn stats(&self) -> (usize, usize) {
        let count = self.total_chars.get().unwrap_or_else(|| {
            let mut sum: usize = 0;
            for (&page_idx, page) in &self.pages {
                let page_base = page_idx << PAGE_SHIFT;
                for offset in 0..PAGE_SIZE {
                    let line = page_base + offset;
                    if line < self.line_count {
                        sum += page[offset].len();
                    }
                }
            }
            self.total_chars.set(Some(sum));
            sum
        });
        (count, self.line_count())
    }

    /// 先頭から指定位置 `at` までの文字数を計算します。
    /// 戻り値: `Some((改行抜文字数, 改行文字数))`。
    /// 途中に未着行（`Line::Absent`）が1行でも含まれる場合は即座に `None` を返します。
    pub fn chars_until(&self, at: Pos) -> Option<(usize, usize)> {
        if at.line >= self.line_count {
            return None;
        }
        let mut chars_without_nl = 0;
        for line in 0..at.line {
            if self.is_absent(line) {
                return None;
            }
            chars_without_nl += self.line_len(line);
        }
        if self.is_absent(at.line) {
            return None;
        }
        let col = at.col.min(self.line_len(at.line));
        chars_without_nl += col;
        let newlines = at.line;
        Some((chars_without_nl, newlines))
    }

    /// 2つの位置 `from` と `to` の間の文字数を計算します。
    /// 戻り値: `Some((改行抜文字数, 改行文字数))`。
    /// 途中に未着行（`Line::Absent`）が1行でも含まれる場合は即座に `None` を返します。
    pub fn chars_between(&self, from: Pos, to: Pos) -> Option<(usize, usize)> {
        let (start, end) = if from <= to { (from, to) } else { (to, from) };
        if start.line >= self.line_count || end.line >= self.line_count {
            return None;
        }
        if start.line == end.line {
            if self.is_absent(start.line) {
                return None;
            }
            let line_len = self.line_len(start.line);
            let col_start = start.col.min(line_len);
            let col_end = end.col.min(line_len);
            let chars = col_end.saturating_sub(col_start);
            return Some((chars, 0));
        }

        if self.is_absent(start.line) {
            return None;
        }
        let start_line_len = self.line_len(start.line);
        let mut chars = start_line_len.saturating_sub(start.col.min(start_line_len));

        for line in start.line + 1..end.line {
            if self.is_absent(line) {
                return None;
            }
            chars += self.line_len(line);
        }

        if self.is_absent(end.line) {
            return None;
        }
        let end_line_len = self.line_len(end.line);
        chars += end.col.min(end_line_len);
        let newlines = end.line - start.line;
        Some((chars, newlines))
    }
}

pub fn before_col(at: Pos) -> Option<Pos> {
    (at.col > 0).then(|| Pos::new(at.line, at.col - 1))
}

pub fn nodes_of(text: &str) -> Vec<Row> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    normalized
        .split('\n')
        .map(|line| line.chars().map(Node::char).collect())
        .collect()
}

pub fn as_char(node: &Node) -> Option<char> {
    match node.kind {
        NodeKind::Char(c) => Some(c),
        _ => None,
    }
}

pub fn character_before(row: &[Node], index: usize) -> usize {
    if index == 0 || row.is_empty() {
        return 0;
    }
    let target = index.min(row.len());
    if target == 0 {
        return 0;
    }
    let min_start = target.saturating_sub(32);
    let mut char_start = target;
    while char_start > min_start && as_char(&row[char_start - 1]).is_some() {
        char_start -= 1;
    }
    if char_start == target {
        return target - 1;
    }
    let s: String = row[char_start..target].iter().filter_map(as_char).collect();
    use unicode_segmentation::UnicodeSegmentation;
    let mut cluster_char_len = 1;
    for g in s.graphemes(true) {
        cluster_char_len = g.chars().count();
    }
    target.saturating_sub(cluster_char_len).max(char_start)
}

pub fn character_after(row: &[Node], index: usize) -> usize {
    if index >= row.len() {
        return row.len();
    }
    let max_end = (index + 32).min(row.len());
    let mut char_end = index;
    while char_end < max_end && as_char(&row[char_end]).is_some() {
        char_end += 1;
    }
    if char_end == index {
        return index + 1;
    }
    let s: String = row[index..char_end].iter().filter_map(as_char).collect();
    use unicode_segmentation::UnicodeSegmentation;
    if let Some(first_grapheme) = s.graphemes(true).next() {
        let cluster_char_len = first_grapheme.chars().count();
        (index + cluster_char_len).min(row.len())
    } else {
        (index + 1).min(row.len())
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
    fn emoji_zwj_family_is_single_grapheme_cluster() {
        let row: Row = "👨‍👩‍👦‍👦".chars().map(Node::char).collect();
        let total_chars = row.len(); // 7 code points with ZWJ
        assert_eq!(character_after(&row, 0), total_chars);
        assert_eq!(character_before(&row, total_chars), 0);
    }

    #[test]
    fn arabic_combining_marks_share_one_character_boundary() {
        let row: Row = "اَلب".chars().map(Node::char).collect();
        assert_eq!(character_after(&row, 0), 2);
        assert_eq!(character_before(&row, 2), 0);
        assert_eq!(character_after(&row, 2), 3);
        assert_eq!(character_before(&row, 4), 3);
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

    #[test]
    fn benchmark_sparse_paging_one_hundred_million_lines() {
        use std::time::Instant;

        // 1. 1億行の pending 作成時間測定
        let t0 = Instant::now();
        let mut text = Text::pending(100_000_000);
        let creation_time = t0.elapsed();

        assert_eq!(text.line_count(), 100_000_000);
        assert_eq!(text.absent_lines(), 100_000_000);
        assert_eq!(text.pages.len(), 0); // 1ページもメモリに確保していない！

        // 2. 1億行中の一部の行にロード（窓: ちょうど 1 ページ分 = 1024 行）
        let base_line = 48828 * PAGE_SIZE;
        let t1 = Instant::now();
        for i in base_line..base_line + PAGE_SIZE {
            text.fill_line(i, SourceLine::Plain("let x = 42;".into()));
        }
        let feed_time = t1.elapsed();

        assert_eq!(text.pages.len(), 1); // ちょうど 1 ページ (1024 行) だけ確保！

        // 3. ランダムアクセス速度測定
        let t2 = Instant::now();
        let mut total_len = 0;
        for i in base_line..base_line + PAGE_SIZE {
            total_len += text.line_len(i);
        }
        let access_time = t2.elapsed();

        assert_eq!(total_len, 1024 * 11);

        // 4. ページ単位の evict_far 解放速度測定
        let t3 = Instant::now();
        text.evict_far(0..10, &[]);
        let evict_time = t3.elapsed();

        assert_eq!(text.pages.len(), 0); // 完全に解放されて 0 ページに！

        println!(
            "\n=== 1024 Paging Sparse Text Benchmark (100,000,000 Lines) ===\n\
             - 1億行の pending 作成時間: {:?}\n\
             - 1024行の fill_line 時間: {:?}\n\
             - 1024行の ランダムアクセス時間: {:?} ({:.2} ns/op)\n\
             - 1024行の evict_far 解放時間: {:?}\n\
             - 1億行時のメモリページ数: 初期 0 ページ、解放後 0 ページ\n",
            creation_time,
            feed_time,
            access_time,
            access_time.as_nanos() as f64 / 1024.0,
            evict_time
        );
    }

    #[test]
    fn chars_until_and_chars_between_calculate_accurately() {
        let lines = vec![
            SourceLine::Plain("hello".into()),
            SourceLine::Plain("world".into()),
            SourceLine::Plain("test line 3".into()),
        ];
        let text = Text::compose(lines);

        // 1. 先頭 (0, 0)
        assert_eq!(text.chars_until(Pos::new(0, 0)), Some((0, 0)));

        // 2. 0行目の中間 (0, 3) -> "hel"
        assert_eq!(text.chars_until(Pos::new(0, 3)), Some((3, 0)));

        // 3. 0行目の行末 (0, 5) -> "hello"
        assert_eq!(text.chars_until(Pos::new(0, 5)), Some((5, 0)));

        // 4. 1行目の行頭 (1, 0) -> "hello" + 改行1
        assert_eq!(text.chars_until(Pos::new(1, 0)), Some((5, 1)));

        // 5. 1行目の中間 (1, 2) -> "hello" + "wo" + 改行1
        assert_eq!(text.chars_until(Pos::new(1, 2)), Some((7, 1)));

        // 6. 2行目の行末 (2, 11) -> 5 + 5 + 11 = 21, 改行2
        assert_eq!(text.chars_until(Pos::new(2, 11)), Some((21, 2)));

        // 7. 単一行間の選択 (0, 1)..(0, 4) -> "ell" (3文字, 改行0)
        assert_eq!(
            text.chars_between(Pos::new(0, 1), Pos::new(0, 4)),
            Some((3, 0))
        );

        // 8. 複数行間の選択 (0, 2)..(2, 4) -> "llo"(3) + "world"(5) + "test"(4) = 12文字, 改行2
        assert_eq!(
            text.chars_between(Pos::new(0, 2), Pos::new(2, 4)),
            Some((12, 2))
        );

        // 9. 逆順の引数 (2, 4)..(0, 2) でも同じ結果
        assert_eq!(
            text.chars_between(Pos::new(2, 4), Pos::new(0, 2)),
            Some((12, 2))
        );
    }

    #[test]
    fn chars_until_and_between_safely_abort_on_absent_lines() {
        let mut text = Text::pending(100_000);
        // 先頭0行目だけロード
        text.fill_line(0, SourceLine::Plain("hello".into()));
        // 50_000行目だけロード
        text.fill_line(50_000, SourceLine::Plain("world".into()));

        // 先頭行内は計算可能
        assert_eq!(text.chars_until(Pos::new(0, 3)), Some((3, 0)));

        // 未着行を含む先頭からの走査は即時 None で安全に中断
        assert_eq!(text.chars_until(Pos::new(50_000, 2)), None);

        // 未着行を跨ぐ選択も即時 None で中断
        assert_eq!(
            text.chars_between(Pos::new(0, 0), Pos::new(50_000, 5)),
            None
        );
    }
}
