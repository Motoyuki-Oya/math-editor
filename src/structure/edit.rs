//! アイランド内のカーソル移動と編集コマンド。

use super::ast::{is_arrow, row_at, row_at_mut, Between, Cursor, Delim, Node, Row};
use super::vocabulary;

/// カーソルが吸収できなかった編集の結果。そのため、周囲のテキスト エディタが代わりに反応する必要があります。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Escape {
    /// キャレットが数式の左端から外れました。
    Left,
    /// キャレットが数式の右端から外れました。
    Right,
    /// 数式は空で、ユーザーが再度 Backspace キーを押しました。
    Delete,
    /// ユーザーは数式を終了するよう要求しました (Escape / Enter)。
    Done,
}

/// アイランドは、編集済み: ドキュメントから借用した構造自体、およびその中を移動するカーソル。
///
/// ここには何もコピーされず、履歴も保持されません。アイランドはドキュメントに属しているため、アイランド内の編集はドキュメントの編集となり、他の編集と同じ履歴によって元に戻されます。
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

    /// キャレットにノードを挿入します。ノードにスロットがある場合、キャレットは最初のスロットに移動します。これにより、パレット ボタンが自然になります。
    pub fn insert(&mut self, node: Node) {
        self.place(node);
    }

    /// 選択範囲を 1 つずつ拡大するか、行の最後に達すると選択範囲が含まれる構造全体を取得します。
    pub fn extend(&mut self, forward: bool) -> Option<Escape> {
        self.stop_waiting();
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

    /// 選択範囲の先頭を `to` に移動します。これが選択範囲のドラッグの方法です。別の行の場所は同じ選択の一部ではないため、推測されずにそのまま残されます。
    pub fn extend_to(&mut self, to: &Cursor) {
        self.stop_waiting();
        if to.path != self.cursor.path {
            return;
        }
        self.cursor.index = to.index.min(self.current_row().len());
    }

    /// キャレットを保持する行内で、キャレットが含まれる構造を選択します。したがって、選択すると文字、その周囲の構造、その周囲の構造、そして最後に数式全体が広がり、周囲のテキストが引き継ぎます。
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

    /// すべて選択の場合は、キャレットが含まれる行全体を選択します。
    pub fn select_row(&mut self) {
        self.stop_waiting();
        self.cursor.anchor = 0;
        self.cursor.index = self.current_row().len();
    }

    /// 単純なキャレットを `index` に配置します。選択以外のすべてがこの方法で終了するため、選択がその選択を行ったコマンドよりも長く存続することはありません。
    fn caret_at(&mut self, index: usize) {
        self.cursor.index = index;
        self.cursor.anchor = index;
    }

    /// キャレットが移動すると、テキスト内で行われるように、移動が進む側に選択が折りたたまれます。
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

    /// 選択した構造を削除するため、選択の上に入力すると、それが置き換えられます。
    fn take_selection(&mut self) -> bool {
        if self.cursor.is_caret() {
            return false;
        }
        let (start, end) = (self.cursor.start(), self.cursor.end());
        self.current_row_mut().drain(start..end);
        self.caret_at(start);
        true
    }

    /// 貼り付けのために、構造の行全体をキャレットに配置します。
    pub fn insert_row(&mut self, nodes: Row) {
        self.stop_waiting();
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
        let waits = waits_for_one(&node);
        let enter = node.slot_count() > 0;
        let index = self.cursor.index;
        self.current_row_mut().insert(index, node);
        if enter {
            self.cursor.path.push((index, 0));
            self.caret_at(0);
            if waits {
                self.wait_for_one();
            }
        } else {
            self.caret_at(self.cursor.index + 1);
        }
    }

    /// キャレットが存在する行がその 1 つのことを待っているかどうかtake.
    fn waiting(&self) -> bool {
        self.cursor.fills.last() == Some(&self.cursor.path.len())
    }

    /// キャレットが入力した行を 1 つのものだけを取るものとしてマークします。
    fn wait_for_one(&mut self) {
        self.cursor.fills.push(self.cursor.path.len());
    }

    /// そこに書き込まれる 1 つのものを待機していたすべての行からキャレットを戻します。これにより、構造体の後に書き込みが続行されます。数式自体は待機状態になる可能性があります。つまり、「1/」と入力して開始された数式は、その分数に対してのみ存在します。その後、キャレットはテキストに戻りますが、これは呼び出し元だけが行うことができます。
    #[must_use]
    fn settle(&mut self, leave_formula: bool) -> Option<Escape> {
        while self.cursor.fills.last() == Some(&self.cursor.path.len()) {
            match self.cursor.path.pop() {
                Some((node, _)) => {
                    self.cursor.fills.pop();
                    self.caret_at(node + 1);
                }
                // 数式は、その上に構築されたものを保持し続けるため、「a/b/c」はスタックされます。通常のテキストが続いて初めて終了します。
                None if !leave_formula => return None,
                None => {
                    self.cursor.fills.pop();
                    return Some(Escape::Right);
                }
            }
        }
        None
    }

    /// キャレットの移動は書き込みではなく編集であるため、その 1 つの処理を待つ行はありません。
    fn stop_waiting(&mut self) {
        self.cursor.fills.clear();
    }

    /// マークダウン エディタがショートカットを展開する方法と同じように、入力したばかりの `\name` (または入力された `√` などのグリフ) を、その名前の構造に変換します。
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
                match vocabulary::node_for(&name) {
                    Some(node) => (start, node),
                    None => return false,
                }
            }
            None => match row.get(index.wrapping_sub(1)) {
                Some(Node::Char(c)) => match vocabulary::node_for_glyph(*c) {
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

    /// 文字が数式の外に属する場合、エスケープを報告します。呼び出し側は、その文字をテキストに書き込みます。ここには書かれていません。
    pub fn insert_char(&mut self, c: char) -> Option<Escape> {
        // 待機中の行には、上の行に持ち上げられる文字 `/` と同じ文字列が含まれます。それ以外の書き込みは構造体の外側で行われるため、「a/b + 1」は入力されたとおりに読み取られます。すぐに開いた括弧は、代わりに `a/(b + 1)` という 1 つのことです。
        if self.waiting()
            && !carries_on(c)
            && !(self.current_row().is_empty() && matches!(c, '(' | '['))
        {
            if let Some(escape) = self.settle(!builds_on(c)) {
                return Some(escape);
            }
        }
        match c {
            '/' => self.insert_stack(Between::Rule),
            c if is_arrow(c) => self.insert_stack(Between::Arrow(c)),
            '^' => self.insert(Node::Sup(Row::new())),
            '_' => self.insert(Node::Sub(Row::new())),
            '(' | '[' => self.insert(Node::Group {
                delim: Delim::from_open(c).unwrap(),
                body: Row::new(),
            }),
            ')' | ']' => return self.leave_group(),
            // グリッドは、キャレットがある列ごとに拡大します。それ以外の `&` は単なる文字です。
            '&' => {
                if !self.grow_matrix(false) {
                    self.insert(Node::Char('&'));
                }
            }
            _ => self.insert(Node::Char(c)),
        }
        None
    }

    /// `/` (または矢印) を入力すると、紙に書くときと同じように、入力した内容がその上に配置されます。
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
        self.wait_for_one();
    }

    /// 区切り文字を閉じると、キャレットが閉じたグループのすぐ前に移動します。これは、キャレットがその内側にある最も内側の括弧です。括弧内の分母から閉じると、すべてが同じように閉じられます。すべてが入力された場合と同様です。 1 行です。
    fn leave_group(&mut self) -> Option<Escape> {
        let mut depth = self.cursor.path.len();
        while depth > 0 {
            let (node, _) = self.cursor.path[depth - 1];
            let parent = &self.cursor.path[..depth - 1];
            if matches!(self.node_at(parent, node), Some(Node::Group { .. })) {
                self.cursor.path.truncate(depth - 1);
                self.cursor.fills.retain(|&at| at <= self.cursor.path.len());
                self.caret_at(node + 1);
                // 外側の行が待っていたのは括弧だけでした。そのため、分数の外側では `a/(b + c) + 1` が続きます。
                return self.settle(true);
            }
            depth -= 1;
        }
        None
    }

    pub fn backspace(&mut self) -> Option<Escape> {
        self.stop_waiting();
        if self.take_selection() {
            return None;
        }
        if self.cursor.index > 0 {
            let index = self.cursor.index - 1;
            let row = self.current_row_mut();
            let node = row[index].clone();
            match node.slot_count() {
                // コンテナを削除すると、その内容が保持されます。ユーザーの作業が破棄されるのではなく、構造が剥がされます。
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
        // スロットの開始時: コンテナの左端まで出ます。
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
        self.stop_waiting();
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
        self.stop_waiting();
        // 選択範囲を離れると、キャレットがその端に配置されるだけです。
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
        self.stop_waiting();
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

    /// 現在のスロットより上 (または下) にスロットがあるコンテナを探してパスを上っていきます: 分子と分母、上限と下限、または行列の隣接する行。
    fn move_vertically(&mut self, up: bool) -> bool {
        self.stop_waiting();
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
        self.stop_waiting();
        self.caret_at(0);
    }

    pub fn move_end(&mut self) {
        self.stop_waiting();
        self.caret_at(self.current_row().len());
    }

    /// 右側の周囲のテキストからフィールドにキャレットが入るときに使用される、数式の最後にキャレットを配置します。
    pub fn move_to_end(&mut self) {
        *self.cursor = Cursor::root(self.root.len());
    }

    pub fn move_to_start(&mut self) {
        *self.cursor = Cursor::root(0);
    }

    /// 現在キャレットが入っている行列に行 (または列) を追加します。
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

/// 入力された文字が基になるかどうかそれに従うのではなく、書き込まれたばかりの構造: `a/b/c` はスタックし、`a/b^2` はスクリプトを取得します。
fn builds_on(c: char) -> bool {
    matches!(c, '/' | '^' | '_') || is_arrow(c)
}

/// 入力された文字が待機行の一部であるかどうか: 実行 `/` 自体が上の行に移動するか、書き込まれているコマンド (`\alpha`、`√`)、またはそれに続く実行にバインドされるスクリプトです。これらは、他の何かが終了しても数式を継続します。
fn carries_on(c: char) -> bool {
    c.is_alphanumeric()
        || matches!(c, '.' | '\\' | '^' | '_')
        || vocabulary::node_for_glyph(c).is_some()
}

/// この構造体が開く行が 1 つのことを実行してからキャレットを戻すかどうか。これらは、1 次元の読み取りで括弧なしで書き込まれる構造 (`√2 + 1`、`x^2 + 1`) なので、書き込みは同じ方法で終了する必要があります。それより長いものは括弧で囲まれます: `√(a + b)`。
fn waits_for_one(node: &Node) -> bool {
    matches!(
        node,
        Node::Sqrt { index: None, .. } | Node::Sup(_) | Node::Sub(_)
    )
}

/// `/` が入力されたときに暗黙的に上の行が始まる場所を見つけます。つまり、キャレットの直前の一連の文字 (または単一のグループ) です。
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

/// キャレットで終わるコマンド ワードを開始する `\` を見つけます。
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
    /// ここでのフィクスチャのみが表記法を通過します。この層のテスト以外には、構造体がどのように書かれているかを知るものは何もありません。
    use crate::format::notation;

    /// 独立したアイランドで、それを保持するドキュメントの代わりをします。
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
        // `+4` は、その下の行に落ちずに分数をたどります。
        assert_eq!(island.to_notation(), "$(1/($(2/3)))+4");
    }

    /// 下の行は、`/` 自体が持ち上げられてキャレットを戻すので、入力された内容は、行に書かれたとおりに読み取られます。
    #[test]
    fn a_lower_row_takes_one_run_and_then_hands_the_caret_back() {
        let mut island = Island::new();
        island.type_in("a/b + 1");
        assert_eq!(island.to_notation(), "$(a/b) + 1");
    }

    #[test]
    fn a_longer_lower_row_is_written_in_brackets() {
        let mut island = Island::new();
        island.type_in("a/(b + c) + 1");
        assert_eq!(island.to_notation(), "$(a/(b + c)) + 1");
    }

    #[test]
    fn a_digit_run_stays_in_the_lower_row() {
        let mut island = Island::new();
        island.type_in("1/12+3");
        assert_eq!(island.to_notation(), "$(1/12)+3");
    }

    #[test]
    fn brackets_after_the_lower_row_are_not_part_of_it() {
        let mut island = Island::new();
        island.type_in("c/d(e/f) +g");
        assert_eq!(island.to_notation(), "$(c/d)($(e/f)) +g");
    }

    #[test]
    fn a_second_slash_stacks_on_the_first_fraction() {
        let mut island = Island::new();
        island.type_in("a/b/c");
        assert_eq!(island.to_notation(), "$(a/b)/c");
    }

    #[test]
    fn a_root_takes_one_run_the_same_way() {
        let mut island = Island::new();
        island.type_in("√");
        assert!(island.edit().commit_command());
        island.type_in("2 + 1");
        assert_eq!(island.to_notation(), "$(√ 2) + 1");
    }

    #[test]
    fn a_script_belongs_to_the_run_it_follows() {
        let mut island = Island::new();
        island.type_in("a/b^2 + 1");
        assert_eq!(island.to_notation(), "$(a/b$(^ 2)) + 1");
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
        // 右から分数に移動すると、その分数に移動します。
        assert_eq!(island.cursor.path, vec![(0, 1)]);
        assert_eq!(island.edit().extend(false), None);
        assert_eq!((island.cursor.start(), island.cursor.end()), (0, 1));
        // その行を越えると、その上の行にある分数が選択されます。
        assert_eq!(island.edit().extend(false), None);
        assert_eq!(island.cursor.path, Vec::new());
        assert_eq!((island.cursor.start(), island.cursor.end()), (0, 1));
        // 最も外側の行を超えると、選択は数式から外れます。
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

    /// ペーストすると、構造がそのまま挿入されます。ショートカットは再度実行されません。
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
