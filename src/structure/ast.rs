//! アイランドの構造とその中を移動するカーソル。
//!
//! アイランドは [`Row`]、つまり [`Node`] のフラットなシーケンスです。サブ行 (スタック、ルートなど) を含むノードは、それらを番号付きの *スロット* として公開するため、ナビゲーションと編集ですべてのコンテナを均一に扱うことができます。

pub type Row = Vec<Node>;

/// スタックの 2 つの行の間に描画できる矢印は、ルールに従って、それらに合わせて引き伸ばされます。
const ARROWS: [char; 8] = ['→', '←', '↔', '⇒', '⇐', '⇔', '⇄', '↦'];

pub fn is_arrow(c: char) -> bool {
    ARROWS.contains(&c)
}

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

/// グリッド `[a, b][c, d]` は、単独で、またはケース分割の中括弧の後ろにあります。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatrixKind {
    Grid,
    Cases,
}

/// 2 つの間に描画されるもの[`Node::stack`] の行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Between {
    /// 幅の広い行の幅を決定します。
    Rule,
    /// 何もありません: 行は単に積み上げられます。
    Nothing,
    /// 幅の広い行の幅まで引き伸ばされた矢印。
    Arrow(char),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub kind: NodeKind,
    pub upper: Row,
    pub lower: Row,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeKind {
    Char(char),
    /// Document-level column separator. Nested rows do not interpret it specially.
    Tab,
    BigOp(String),
    Stack {
        above: Row,
        below: Row,
        between: Between,
    },
    Sqrt {
        index: Option<Row>,
        body: Row,
    },
    Sup(Row),
    Sub(Row),
    Group {
        delim: Delim,
        body: Row,
    },
    Container(Row),
    Matrix {
        kind: MatrixKind,
        cells: Vec<Vec<Row>>,
    },
}

impl Node {
    pub fn new(kind: NodeKind) -> Node {
        Node {
            kind,
            upper: Row::new(),
            lower: Row::new(),
        }
    }

    pub fn char(c: char) -> Node {
        Node::new(NodeKind::Char(c))
    }
    pub fn tab() -> Node {
        Node::new(NodeKind::Tab)
    }
    pub fn big_op(name: String) -> Node {
        Node::new(NodeKind::BigOp(name))
    }
    pub fn stack(above: Row, below: Row, between: Between) -> Node {
        Node::new(NodeKind::Stack {
            above,
            below,
            between,
        })
    }
    pub fn sqrt(index: Option<Row>, body: Row) -> Node {
        Node::new(NodeKind::Sqrt { index, body })
    }
    pub fn sup(row: Row) -> Node {
        Node::new(NodeKind::Sup(row))
    }
    pub fn sub(row: Row) -> Node {
        Node::new(NodeKind::Sub(row))
    }
    pub fn group(delim: Delim, body: Row) -> Node {
        Node::new(NodeKind::Group { delim, body })
    }
    pub fn container(row: Row) -> Node {
        Node::new(NodeKind::Container(row))
    }
    pub fn matrix(kind: MatrixKind, cells: Vec<Vec<Row>>) -> Node {
        Node::new(NodeKind::Matrix { kind, cells })
    }

    pub fn intrinsic_slot_count(&self) -> usize {
        match &self.kind {
            NodeKind::Char(_) | NodeKind::Tab | NodeKind::BigOp(_) => 0,
            NodeKind::Stack { .. } => 2,
            NodeKind::Sqrt { index, .. } => usize::from(index.is_some()) + 1,
            NodeKind::Sup(_)
            | NodeKind::Sub(_)
            | NodeKind::Group { .. }
            | NodeKind::Container(_) => 1,
            NodeKind::Matrix { cells, .. } => cells.iter().map(Vec::len).sum(),
        }
    }

    pub fn lower_slot(&self) -> usize {
        self.intrinsic_slot_count()
    }
    pub fn upper_slot(&self) -> usize {
        self.intrinsic_slot_count() + 1
    }
    pub fn slot_count(&self) -> usize {
        self.intrinsic_slot_count() + 2
    }

    pub fn slot(&self, i: usize) -> Option<&Row> {
        let intrinsic = self.intrinsic_slot_count();
        if i == intrinsic {
            return Some(&self.lower);
        }
        if i == intrinsic + 1 {
            return Some(&self.upper);
        }
        match &self.kind {
            NodeKind::Stack { above, below, .. } => [above, below].get(i).copied(),
            NodeKind::Sqrt { index, body } => match (index, i) {
                (Some(index), 0) => Some(index),
                (Some(_), 1) | (None, 0) => Some(body),
                _ => None,
            },
            NodeKind::Sup(row)
            | NodeKind::Sub(row)
            | NodeKind::Group { body: row, .. }
            | NodeKind::Container(row) => (i == 0).then_some(row),
            NodeKind::Matrix { cells, .. } => cells.iter().flatten().nth(i),
            _ => None,
        }
    }

    pub fn slot_mut(&mut self, i: usize) -> Option<&mut Row> {
        let intrinsic = self.intrinsic_slot_count();
        if i == intrinsic {
            return Some(&mut self.lower);
        }
        if i == intrinsic + 1 {
            return Some(&mut self.upper);
        }
        match &mut self.kind {
            NodeKind::Stack { above, below, .. } => match i {
                0 => Some(above),
                1 => Some(below),
                _ => None,
            },
            NodeKind::Sqrt { index, body } => match (index.is_some(), i) {
                (true, 0) => index.as_mut(),
                (true, 1) | (false, 0) => Some(body),
                _ => None,
            },
            NodeKind::Sup(row)
            | NodeKind::Sub(row)
            | NodeKind::Group { body: row, .. }
            | NodeKind::Container(row) => (i == 0).then_some(row),
            NodeKind::Matrix { cells, .. } => cells.iter_mut().flatten().nth(i),
            _ => None,
        }
    }

    pub fn horizontal_slots(&self) -> Vec<usize> {
        let mut slots: Vec<usize> = (0..self.intrinsic_slot_count()).collect();
        if matches!(&self.kind, NodeKind::BigOp(_)) {
            if !self.lower.is_empty() {
                slots.push(self.lower_slot());
            }
            if !self.upper.is_empty() {
                slots.push(self.upper_slot());
            }
        }
        slots
    }

    pub fn matrix_shape(&self) -> Option<(usize, usize)> {
        match &self.kind {
            NodeKind::Matrix { cells, .. } => {
                Some((cells.len(), cells.first().map(Vec::len).unwrap_or(0)))
            }
            _ => None,
        }
    }
}

/// 式内の位置: ルート行から取得した (ノード、スロット) ホップのチェーンと、そのチェーンがつながる行内のオフセット。
///
/// 選択としても機能する。テキスト内のキャレットは次のようになります: `anchor` が選択を開始する場所です。選択範囲は、それが開始された行内に残ります。どちらかの端を越えて到達すると、その行が属する構造が代わりに選択されます。これにより、構造全体が取得されます。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cursor {
    pub path: Vec<(usize, usize)>,
    pub index: usize,
    pub anchor: usize,
    /// 行に書き込まれる 1 つの内容を待機している行の深さ。 `/` を入力すると下の行が開き、次に書き込まれた内容が取り込まれてからキャレットが戻されるため、 `a/b + 1` は入力されたとおりに読み取ります。 'a/(b + 1)' のように、長い下の行は括弧で囲まれます。キャレットを移動すると待機が終了します。これ以降、ユーザーは書き込みではなく編集を行うためです。
    pub fills: Vec<usize>,
}

impl Cursor {
    pub fn root(index: usize) -> Cursor {
        Cursor {
            path: Vec::new(),
            index,
            anchor: index,
            fills: Vec::new(),
        }
    }

    pub fn is_caret(&self) -> bool {
        self.anchor == self.index
    }

    pub fn start(&self) -> usize {
        self.anchor.min(self.index)
    }

    pub fn end(&self) -> usize {
        self.anchor.max(self.index)
    }
}

pub fn row_at<'a>(root: &'a [Node], path: &[(usize, usize)]) -> Option<&'a [Node]> {
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

pub fn stack(between: Between) -> Node {
    Node::stack(empty_row(), empty_row(), between)
}

pub fn sqrt() -> Node {
    Node::sqrt(None, empty_row())
}

pub fn nth_root() -> Node {
    Node::sqrt(Some(empty_row()), empty_row())
}

pub fn limits(sym: &str) -> Node {
    Node::big_op(sym.to_string())
}

pub fn matrix(kind: MatrixKind, rows: usize, cols: usize) -> Node {
    Node::matrix(
        kind,
        (0..rows)
            .map(|_| (0..cols).map(|_| empty_row()).collect())
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_node_owns_universal_annotation_rows() {
        let mut leaf = Node::char('x');
        assert!(leaf.upper.is_empty());
        assert!(leaf.lower.is_empty());
        leaf.upper.push(Node::char('n'));
        assert_eq!(leaf.slot(leaf.upper_slot()), Some(&leaf.upper));
    }

    #[test]
    fn slots_are_addressable() {
        let node = stack(Between::Rule);
        assert_eq!(node.slot_count(), 4);
        assert!(node.slot(0).is_some());
        assert!(node.slot(2).is_some());
        assert!(node.slot(4).is_none());
    }

    #[test]
    fn sqrt_index_shifts_slots() {
        let plain = sqrt();
        assert_eq!(plain.intrinsic_slot_count(), 1);
        let nth = nth_root();
        assert_eq!(nth.intrinsic_slot_count(), 2);
    }

    #[test]
    fn rows_resolve_through_paths() {
        let root: Row = vec![Node::stack(
            vec![Node::char('a')],
            vec![Node::char('b')],
            Between::Rule,
        )];
        assert_eq!(row_at(&root, &[(0, 1)]), Some(&[Node::char('b')][..]));
        assert_eq!(row_at(&root, &[(0, 5)]), None);
    }

    #[test]
    fn matrix_slots_are_row_major() {
        let node = matrix(MatrixKind::Grid, 2, 2);
        assert_eq!(node.intrinsic_slot_count(), 4);
        assert_eq!(node.slot_count(), 6);
        assert_eq!(node.matrix_shape(), Some((2, 2)));
    }
}
