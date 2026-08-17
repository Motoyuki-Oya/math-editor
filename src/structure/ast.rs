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

/// 2 つの間に描画されるもの[`Node::Stack`] の行。
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
pub enum Node {
    /// 直接入力された文字: 変数、数字、または演算子。
    Char(char),
    /// `\alpha` や `\leq` などの名前付きシンボル。バックスラッシュ。
    Sym(String),
    /// バックスラッシュなしで保存される、`\sin` などの直立関数名。
    Func(String),
    /// 上と下に、ルール、矢印、または何も描かれていないもの。
    Stack {
        above: Row,
        below: Row,
        between: Between,
    },
    Sqrt {
        index: Option<Row>,
        body: Row,
    },
    /// 行内でその前にあるものに付けられた上付き文字。
    Sup(Row),
    /// 行内でその前に付けられた下付き文字。
    Sub(Row),
    Group {
        delim: Delim,
        body: Row,
    },
    /// 上下に何か書かれたシンボル。
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

    /// カーソルが着地するスロット左からノードに入るとき。
    pub fn entry_slot(&self) -> usize {
        0
    }

    /// 右からノードに入るとき、カーソルが着地するスロット。
    pub fn exit_slot(&self) -> usize {
        match self {
            // スタックは右からその下の行に入る。
            Node::Stack { .. } => 1,
            other => other.slot_count().saturating_sub(1),
        }
    }

    /// ノードが行列の場合、行列の次元。
    pub fn matrix_shape(&self) -> Option<(usize, usize)> {
        match self {
            Node::Matrix { cells, .. } => {
                Some((cells.len(), cells.first().map(|r| r.len()).unwrap_or(0)))
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

pub fn stack(between: Between) -> Node {
    Node::Stack {
        above: empty_row(),
        below: empty_row(),
        between,
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
        let node = stack(Between::Rule);
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
            between: Between::Rule,
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
