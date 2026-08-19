//! 文書行と入れ子Rowで共通に使うカーソル移動・編集コマンド。

use super::ast::{is_arrow, row_at, row_at_mut, Between, Cursor, Delim, Node, NodeKind, Row};
use super::vocabulary;

/// カーソルが吸収できなかった編集の結果。そのため、周囲のテキスト エディタが代わりに反応する必要があります。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Escape {
    /// キャレットが編集中のRowの左端から外れました。
    Left,
    /// キャレットが編集中のRowの右端から外れました。
    Right,
    /// Rowが空で、ユーザーが再度 Backspace キーを押しました。
    Delete,
}

/// ドキュメントから借用したRowと、その中を移動するCursorへ編集操作を適用します。
///
/// Rowはコピーせずその場で変更し、履歴は呼び出し元のEditorが管理します。
pub struct Editing<'a> {
    pub root: &'a mut Row,
    pub cursor: &'a mut Cursor,
}

impl<'a> Editing<'a> {
    pub fn new(root: &'a mut Row, cursor: &'a mut Cursor) -> Editing<'a> {
        Editing { root, cursor }
    }

    fn current_row(&self) -> &[Node] {
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

    /// 選択を現在の文字から、それを含む構造、さらに外側の構造へ順に広げます。
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
        let entry = if matches!(&node.kind, NodeKind::BigOp(_)) {
            Some(node.lower_slot())
        } else {
            node.horizontal_slots().first().copied()
        };
        let index = self.cursor.index;
        self.current_row_mut().insert(index, node);
        if let Some(entry) = entry {
            self.cursor.path.push((index, entry));
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

    /// 1まとまりの入力を待っていた内側Rowから、待機元のRowへキャレットを戻します。
    #[must_use]
    fn settle(&mut self, leave_root: bool) -> Option<Escape> {
        while self.cursor.fills.last() == Some(&self.cursor.path.len()) {
            match self.cursor.path.pop() {
                Some((node, _)) => {
                    self.cursor.fills.pop();
                    self.caret_at(node + 1);
                }
                // `/`やスクリプトが続く場合は、直前に作った構造へさらに積み重ねます。
                None if !leave_root => return None,
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
                    .filter_map(|node| match &node.kind {
                        NodeKind::Char(c) => Some(*c),
                        _ => None,
                    })
                    .collect();
                if let Some(node) = vocabulary::structure_for(&name) {
                    (start, node)
                } else if let Some(text) = vocabulary::text_for(&name) {
                    self.current_row_mut().drain(start..index);
                    self.caret_at(start);
                    self.insert_row(text.chars().map(Node::char).collect());
                    return true;
                } else {
                    return false;
                }
            }
            None => match row.get(index.wrapping_sub(1)).map(|node| &node.kind) {
                Some(NodeKind::Char(c)) => match vocabulary::node_for_glyph(*c) {
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

    /// 待機中の構造を抜ける文字ではEscapeを返し、呼び出し側が外側Rowへ入力します。
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
            '^' => self.insert(Node::sup(Row::new())),
            '_' => self.insert(Node::sub(Row::new())),
            '(' | '[' => self.insert(Node::group(Delim::from_open(c).unwrap(), Row::new())),
            ')' | ']' => return self.leave_group(),
            // グリッドは、キャレットがある列ごとに拡大します。それ以外の `&` は単なる文字です。
            '&' => {
                if !self.grow_matrix(false) {
                    self.insert(Node::char('&'));
                }
            }
            _ => self.insert(Node::char(c)),
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
        let node = Node::stack(above, Row::new(), between);
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
            if self
                .node_at(parent, node)
                .is_some_and(|node| matches!(&node.kind, NodeKind::Group { .. }))
            {
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
            if let Some(slot) = node.horizontal_slots().last().copied() {
                let len = node.slot(slot).map(Vec::len).unwrap_or(0);
                self.cursor.path.push((index, slot));
                self.caret_at(len);
            } else {
                self.caret_at(index);
            }
            return None;
        }
        match self.cursor.path.pop() {
            Some((node, slot)) => {
                let parent = self.cursor.path.clone();
                let previous = self.node_at(&parent, node).and_then(|n| {
                    let slots = n.horizontal_slots();
                    slots
                        .iter()
                        .position(|candidate| *candidate == slot)
                        .and_then(|at| at.checked_sub(1).map(|before| slots[before]))
                });
                if let Some(previous) = previous {
                    let len = self
                        .node_at(&parent, node)
                        .and_then(|n| n.slot(previous))
                        .map(Vec::len)
                        .unwrap_or(0);
                    self.cursor.path.push((node, previous));
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
            if let Some(slot) = node.horizontal_slots().first().copied() {
                self.cursor.path.push((index, slot));
                self.caret_at(0);
            } else {
                self.caret_at(index + 1);
            }
            return None;
        }
        match self.cursor.path.pop() {
            Some((node, slot)) => {
                let parent = self.cursor.path.clone();
                let next = self.node_at(&parent, node).and_then(|n| {
                    let slots = n.horizontal_slots();
                    slots
                        .iter()
                        .position(|candidate| *candidate == slot)
                        .and_then(|at| slots.get(at + 1).copied())
                });
                if let Some(next) = next {
                    self.cursor.path.push((node, next));
                    self.caret_at(0);
                } else {
                    self.caret_at(node + 1);
                }
                None
            }
            None => Some(Escape::Right),
        }
    }

    pub fn annotate(&mut self, upper: bool) -> bool {
        self.stop_waiting();
        let (start, end) = if self.cursor.is_caret() {
            let len = self.current_row().len();
            if self.cursor.index > 0 {
                (self.cursor.index - 1, self.cursor.index)
            } else if self.cursor.index < len {
                (self.cursor.index, self.cursor.index + 1)
            } else {
                return false;
            }
        } else {
            (self.cursor.start(), self.cursor.end())
        };
        let selected: Row = self.current_row_mut().drain(start..end).collect();
        let node = if selected.len() == 1 {
            selected.into_iter().next().expect("one selected node")
        } else {
            Node::container(selected)
        };
        let slot = if upper {
            node.upper_slot()
        } else {
            node.lower_slot()
        };
        self.current_row_mut().insert(start, node);
        self.cursor.path.push((start, slot));
        self.caret_at(0);
        true
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
        let adjacent = if self.cursor.index > 0 {
            Some(self.cursor.index - 1)
        } else if self.cursor.index < self.current_row().len() {
            Some(self.cursor.index)
        } else {
            None
        };
        if let Some(node_index) = adjacent {
            let node = self.current_row()[node_index].clone();
            let slot = if up {
                node.upper_slot()
            } else {
                node.lower_slot()
            };
            if node.slot(slot).is_some_and(|row| !row.is_empty()) {
                self.cursor.path.push((node_index, slot));
                self.caret_at(0);
                return true;
            }
        }
        for depth in (0..self.cursor.path.len()).rev() {
            let (node_index, slot) = self.cursor.path[depth];
            let parent_path = self.cursor.path[..depth].to_vec();
            let Some(node) = self.node_at(&parent_path, node_index).cloned() else {
                continue;
            };
            let lower = node.lower_slot();
            let upper = node.upper_slot();
            let in_annotation = slot == upper || slot == lower;
            let annotation = match (up, in_annotation) {
                (true, false) if !node.upper.is_empty() => Some(upper),
                (false, false) if !node.lower.is_empty() => Some(lower),
                _ => None,
            };
            let intrinsic = match &node.kind {
                NodeKind::Stack { .. } => match (up, slot) {
                    (true, 1) => Some(0),
                    (false, 0) => Some(1),
                    _ => None,
                },
                NodeKind::Matrix { cells, .. } => {
                    let cols = cells.first().map(Vec::len).unwrap_or(1).max(1);
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
            let target = intrinsic.or(annotation);
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
            if (up && slot == lower) || (!up && slot == upper) {
                self.cursor.path.truncate(depth);
                let base = if up {
                    node.horizontal_slots().last().copied()
                } else {
                    node.horizontal_slots().first().copied()
                };
                if let Some(base) = base {
                    self.cursor.path.push((node_index, base));
                    let index = node
                        .slot(base)
                        .map(|row| row.len().min(self.cursor.index))
                        .unwrap_or(0);
                    self.caret_at(index);
                } else {
                    self.caret_at(node_index + usize::from(!up));
                }
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

    /// 右側から構造へ入るとき、編集中のRowの末尾へキャレットを置きます。
    #[cfg(test)]
    pub fn move_to_end(&mut self) {
        *self.cursor = Cursor::root(self.root.len());
    }

    #[cfg(test)]
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
            let Some(Node {
                kind: NodeKind::Matrix { cells, .. },
                ..
            }) = row.get_mut(node_index)
            else {
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

/// 入力された文字が、現在の構造を作る1まとまりとして続くかを判定します。
fn carries_on(c: char) -> bool {
    c.is_alphanumeric()
        || matches!(c, '.' | '\\' | '^' | '_')
        || vocabulary::node_for_glyph(c).is_some()
}

/// この構造体が開く行が 1 つのことを実行してからキャレットを戻すかどうか。これらは、1 次元の読み取りで括弧なしで書き込まれる構造 (`√2 + 1`、`x^2 + 1`) なので、書き込みは同じ方法で終了する必要があります。それより長いものは括弧で囲まれます: `√(a + b)`。
fn waits_for_one(node: &Node) -> bool {
    matches!(
        &node.kind,
        NodeKind::Sqrt { index: None, .. } | NodeKind::Sup(_) | NodeKind::Sub(_)
    )
}

/// `/` が入力されたときに暗黙的に上の行が始まる場所を見つけます。つまり、キャレットの直前の一連の文字 (または単一のグループ) です。
fn above_start(row: &[Node], index: usize) -> usize {
    if index == 0 {
        return 0;
    }
    match &row[index - 1].kind {
        NodeKind::Char(c) if c.is_alphanumeric() || *c == '.' => {
            let mut start = index - 1;
            while start > 0 {
                match &row[start - 1].kind {
                    NodeKind::Char(c) if c.is_alphanumeric() || *c == '.' => start -= 1,
                    _ => break,
                }
            }
            start
        }
        _ => index - 1,
    }
}

/// キャレットで終わるコマンド ワードを開始する `\` を見つけます。
fn command_start(row: &[Node], index: usize) -> Option<usize> {
    let mut start = index;
    while start > 0 {
        match &row[start - 1].kind {
            NodeKind::Char(c) if c.is_ascii_alphabetic() => start -= 1,
            NodeKind::Char('\\') => return (start < index).then_some(start - 1),
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

    /// 編集対象のRowとCursorをまとめたテスト用フィクスチャ。
    struct Fixture {
        root: Row,
        cursor: Cursor,
    }

    impl Fixture {
        fn new() -> Fixture {
            Fixture {
                root: Row::new(),
                cursor: Cursor::default(),
            }
        }

        fn from_notation(source: &str) -> Fixture {
            let root = notation::parse_island(source);
            let cursor = Cursor::root(root.len());
            Fixture { root, cursor }
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
        let mut island = Fixture::new();
        island.type_in("x+1");
        assert_eq!(island.to_notation(), "x+1");
    }

    #[test]
    fn backslash_shortcut_expands_into_a_structure() {
        let mut island = Fixture::new();
        island.type_in("\\sqrt");
        assert!(island.edit().commit_command());
        island.type_in("2");
        assert_eq!(island.to_notation(), "√ 2");
    }

    #[test]
    fn typed_glyph_expands_like_its_command() {
        let mut island = Fixture::new();
        island.type_in("√");
        assert!(island.edit().commit_command());
        island.type_in("2");
        assert_eq!(island.to_notation(), "√ 2");
    }

    #[test]
    fn unknown_backslash_shortcut_is_left_alone() {
        let mut island = Fixture::new();
        island.type_in("\\nope");
        assert!(!island.edit().commit_command());
    }

    #[test]
    fn slash_takes_the_preceding_run_as_the_upper_row() {
        let mut island = Fixture::new();
        island.type_in("1+ab/");
        island.type_in("2c");
        assert_eq!(island.to_notation(), "1+$(ab/2c)");
    }

    #[test]
    fn a_closing_bracket_leaves_the_brackets_from_inside_a_fraction() {
        let mut island = Fixture::new();
        island.type_in("1/(2/3)+4");
        // `+4` は、その下の行に落ちずに分数をたどります。
        assert_eq!(island.to_notation(), "$(1/($(2/3)))+4");
    }

    /// 下の行は、`/` 自体が持ち上げられてキャレットを戻すので、入力された内容は、行に書かれたとおりに読み取られます。
    #[test]
    fn a_lower_row_takes_one_run_and_then_hands_the_caret_back() {
        let mut island = Fixture::new();
        island.type_in("a/b + 1");
        assert_eq!(island.to_notation(), "$(a/b) + 1");
    }

    #[test]
    fn a_longer_lower_row_is_written_in_brackets() {
        let mut island = Fixture::new();
        island.type_in("a/(b + c) + 1");
        assert_eq!(island.to_notation(), "$(a/(b + c)) + 1");
    }

    #[test]
    fn a_digit_run_stays_in_the_lower_row() {
        let mut island = Fixture::new();
        island.type_in("1/12+3");
        assert_eq!(island.to_notation(), "$(1/12)+3");
    }

    #[test]
    fn brackets_after_the_lower_row_are_not_part_of_it() {
        let mut island = Fixture::new();
        island.type_in("c/d(e/f) +g");
        assert_eq!(island.to_notation(), "$(c/d)($(e/f)) +g");
    }

    #[test]
    fn a_second_slash_stacks_on_the_first_fraction() {
        let mut island = Fixture::new();
        island.type_in("a/b/c");
        assert_eq!(island.to_notation(), "$(a/b)/c");
    }

    #[test]
    fn a_root_takes_one_run_the_same_way() {
        let mut island = Fixture::new();
        island.type_in("√");
        assert!(island.edit().commit_command());
        island.type_in("2 + 1");
        assert_eq!(island.to_notation(), "$(√ 2) + 1");
    }

    #[test]
    fn a_script_belongs_to_the_run_it_follows() {
        let mut island = Fixture::new();
        island.type_in("a/b^2 + 1");
        assert_eq!(island.to_notation(), "$(a/b$(^ 2)) + 1");
    }

    #[test]
    fn a_closing_bracket_with_no_brackets_open_changes_nothing() {
        let mut island = Fixture::new();
        island.type_in("a)b");
        assert_eq!(island.to_notation(), "ab");
    }

    #[test]
    fn caret_enters_and_leaves_a_stack() {
        let mut island = Fixture::from_notation("a/b");
        island.edit().move_to_start();
        assert_eq!(island.edit().move_right(), None);
        assert_eq!(island.cursor.path, vec![(0, 0)]);
        island.edit().move_right();
        island.edit().move_right();
        assert_eq!(island.cursor.path, vec![(0, 1)]);
    }

    #[test]
    fn up_and_down_switch_between_the_upper_and_lower_row() {
        let mut island = Fixture::from_notation("a/b");
        island.edit().move_to_start();
        island.edit().move_right();
        assert!(island.edit().move_down());
        assert_eq!(island.cursor.path, vec![(0, 1)]);
        assert!(island.edit().move_up());
        assert_eq!(island.cursor.path, vec![(0, 0)]);
    }

    #[test]
    fn backspace_keeps_the_content_of_a_deleted_structure() {
        let mut island = Fixture::from_notation("ab/c");
        island.edit().move_to_end();
        assert_eq!(island.edit().backspace(), None);
        assert_eq!(island.to_notation(), "abc");
    }

    #[test]
    fn backspace_reports_escape_on_an_empty_root_row() {
        let mut island = Fixture::new();
        assert_eq!(island.edit().backspace(), Some(Escape::Delete));
    }

    #[test]
    fn arrow_past_the_edge_reports_escape() {
        let mut island = Fixture::from_notation("x");
        island.edit().move_to_end();
        assert_eq!(island.edit().move_right(), Some(Escape::Right));
        island.edit().move_to_start();
        assert_eq!(island.edit().move_left(), Some(Escape::Left));
    }

    #[test]
    fn closing_paren_steps_out_of_the_group() {
        let mut island = Fixture::new();
        island.type_in("(x)+");
        assert_eq!(island.to_notation(), "(x)+");
    }

    #[test]
    fn selecting_reaches_the_whole_row_then_the_structure_around_it() {
        let mut island = Fixture::from_notation("1/2");
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
        let mut island = Fixture::from_notation("ab");
        island.edit().select_row();
        assert_eq!((island.cursor.start(), island.cursor.end()), (0, 2));
        island.edit().backspace();
        assert_eq!(island.to_notation(), "");
        assert!(island.cursor.is_caret());
    }

    /// ペーストすると、構造がそのまま挿入されます。ショートカットは再度実行されません。
    #[test]
    fn a_pasted_row_goes_in_at_the_caret() {
        let mut island = Fixture::from_notation("x");
        island.edit().insert_row(notation::parse_island("1/2"));
        assert_eq!(island.to_notation(), "x$(1/2)");
        assert!(island.cursor.is_caret());
        assert_eq!(island.cursor.index, 2);
    }

    #[test]
    fn a_paste_replaces_the_selection() {
        let mut island = Fixture::from_notation("ab");
        island.edit().select_row();
        island.edit().insert_row(notation::parse_island("c"));
        assert_eq!(island.to_notation(), "c");
    }

    #[test]
    fn vertical_movement_skips_empty_and_enters_nonempty_annotations() {
        let mut island = Fixture::from_notation("x");
        assert!(!island.edit().move_up());
        island.root[0].upper = vec![Node::char('n')];
        island.cursor = Cursor::root(1);
        assert!(island.edit().move_up());
        assert_eq!(island.cursor.path, vec![(0, island.root[0].upper_slot())]);
        assert!(island.edit().move_down());
        assert_eq!(island.cursor, Cursor::root(1));
    }

    #[test]
    fn explicit_annotation_enters_an_empty_slot() {
        let mut island = Fixture::from_notation("ab");
        island.cursor.anchor = 0;
        assert!(island.edit().annotate(true));
        assert!(island.root[0].upper.is_empty());
        assert_eq!(island.cursor.path, vec![(0, island.root[0].upper_slot())]);
    }

    #[test]
    fn moving_from_an_annotation_returns_to_the_wrapped_base() {
        let mut island = Fixture::from_notation("abc");
        island.edit().select_row();
        assert!(island.edit().annotate(true));
        island.root[0].lower = vec![Node::char('l')];
        island.edit().insert_char('n');
        assert!(island.edit().move_down());
        assert_eq!(island.cursor.path, vec![(0, 0)]);
    }

    #[test]
    fn inserting_a_big_operator_enters_its_empty_lower_annotation() {
        let mut island = Fixture::new();
        island.edit().insert(Node::big_op("∑".into()));
        assert_eq!(island.cursor.path, vec![(0, island.root[0].lower_slot())]);
    }

    #[test]
    fn horizontal_movement_does_not_enter_annotations() {
        let mut island = Fixture::from_notation("x");
        island.root[0].upper = vec![Node::char('n')];
        island.cursor = Cursor::root(0);
        assert_eq!(island.edit().move_right(), None);
        assert_eq!(island.cursor, Cursor::root(1));
        assert_eq!(island.edit().move_left(), None);
        assert_eq!(island.cursor, Cursor::root(0));
    }

    #[test]
    fn matrix_grows_by_row_and_column() {
        let mut island = Fixture::new();
        island.edit().insert(super::super::ast::matrix(
            super::super::ast::MatrixKind::Grid,
            1,
            2,
        ));
        assert!(island.edit().grow_matrix(true));
        match &island.root[0] {
            Node {
                kind: NodeKind::Matrix { cells, .. },
                ..
            } => assert_eq!(cells.len(), 2),
            other => panic!("expected a matrix, got {other:?}"),
        }
        assert!(island.edit().grow_matrix(false));
        match &island.root[0] {
            Node {
                kind: NodeKind::Matrix { cells, .. },
                ..
            } => assert_eq!(cells[0].len(), 3),
            other => panic!("expected a matrix, got {other:?}"),
        }
    }
}
