//! ドキュメントの編集: 選択内容と、適用されるコマンド。
//!
//! すべてのコマンドは、複数のカーソルが予期される動作として、単一のステップとしてすべての選択に適用されます。ドキュメント自体は [`crate::structure::text`] であり、表記や画面については何も知りません。編集は行の入れ替えとして控えられ、文書の本体（元に戻す履歴を持つ側）へ引き渡されます。

mod history;
mod nested;

pub use history::Flush;
use history::{Recorder, Step};
pub use nested::Inside;

use super::clipboard::Clip;
use crate::structure::ast::{Cursor, Node, Row};
use crate::structure::text::{as_char, nodes_of, Pos, Sel, Text};

/// `to` までのテキストが `end` で終わるテキストに置き換えられると、`p​​os` が終了します。
fn shifted(pos: Pos, to: Pos, end: Pos) -> Pos {
    if pos <= to {
        // 編集が飲み込んだものはすべて最後に残ります。
        return end;
    }
    let line = (pos.line + end.line).saturating_sub(to.line);
    if pos.line == to.line {
        Pos::new(line, pos.col - to.col + end.col)
    } else {
        Pos::new(line, pos.col)
    }
}

/// コマンドが何をしたか、つまり呼び出し元が反応するために必要なのはそれだけです。質問するモードはなく、何かが移動または変更されたかどうかだけを尋ねます。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Did {
    /// 鍵はここには何も意味しませんので、他の誰にも所属しています。
    Nothing,
    /// キャレットまたは選択範囲が移動しました。
    Moved,
    /// 書類が変わりました。
    Changed,
}

impl Did {
    fn moved(happened: bool) -> Did {
        if happened {
            Did::Moved
        } else {
            Did::Nothing
        }
    }
}

/// 本文と入れ子構造を同じ配列で持つ選択。`sel` は文書上の行と構造Nodeの
/// 位置を示し、`inside` があればそこから入れ子Rowまでの絶対パスを示す。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnifiedCursor {
    pub sel: Sel,
    pub inside: Option<Cursor>,
    /// 本文トリガーが今回構造を作る文書行内の位置。
    transient_structure: Option<usize>,
}

impl UnifiedCursor {
    fn caret(at: Pos) -> Self {
        Self {
            sel: Sel::caret(at),
            inside: None,
            transient_structure: None,
        }
    }

    fn range(from: Pos, to: Pos) -> Self {
        Self {
            sel: Sel::range(from, to),
            inside: None,
            transient_structure: None,
        }
    }
}

impl std::ops::Deref for UnifiedCursor {
    type Target = Sel;

    fn deref(&self) -> &Self::Target {
        &self.sel
    }
}

impl std::ops::DerefMut for UnifiedCursor {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.sel
    }
}

pub struct Editor {
    text: Text,
    cursors: Vec<UnifiedCursor>,
    recorder: Recorder,
}

impl Default for Editor {
    fn default() -> Self {
        Self {
            text: Text::default(),
            cursors: vec![UnifiedCursor::caret(Pos::default())],
            recorder: Recorder::default(),
        }
    }
}

impl Editor {
    pub fn text(&self) -> &Text {
        &self.text
    }

    pub fn cursors(&self) -> &[UnifiedCursor] {
        &self.cursors
    }

    /// 描画など本文の選択だけを見る境界で使う。
    pub fn sels(&self) -> Vec<Sel> {
        self.cursors
            .iter()
            .filter(|cursor| cursor.inside.is_none())
            .map(|cursor| cursor.sel)
            .collect()
    }

    /// 焦点を維持する選択。最後に追加したもの。
    pub fn primary(&self) -> Sel {
        self.primary_cursor().sel
    }

    fn primary_cursor(&self) -> &UnifiedCursor {
        self.cursors.last().expect("at least one selection")
    }

    fn primary_cursor_mut(&mut self) -> &mut UnifiedCursor {
        self.cursors.last_mut().expect("at least one selection")
    }

    fn has_inside(&self) -> bool {
        self.cursors.iter().any(|cursor| cursor.inside.is_some())
    }

    fn clear_inside(&mut self) {
        for cursor in &mut self.cursors {
            cursor.inside = None;
            cursor.transient_structure = None;
        }
    }

    /// ファイルから読み取られたばかりのドキュメントを表示します。
    pub fn load(&mut self, text: Text) {
        self.text = text;
        self.cursors = vec![UnifiedCursor::caret(Pos::default())];
        self.recorder = Recorder::default();
    }

    /// 読み込んだ内容をまるごと文書の本体へ届くようにする。本体が
    /// 1 行の空文書のときに使う（下書きの復元）。
    pub fn load_contents(&mut self, text: Text) {
        self.load(text);
        self.record(Step::Other);
        self.text.mark_all_changed();
    }

    /// 行数だけ分かっている文書を表示し、行は見えた場所から届く。
    pub fn load_pending(&mut self, line_count: usize) {
        self.load(Text::pending(line_count));
    }

    /// 走査で確定した行数へ合わせる。
    pub fn resize_pending(&mut self, line_count: usize) {
        self.text.resize_pending(line_count);
    }

    /// 手元に届いた行の数。捨てるかどうかの見分けに使う。
    pub fn resident_lines(&self) -> usize {
        self.text.line_count() - self.text.absent_lines()
    }

    /// `keep` から遠い未編集の行を手放す。選択とキャレットの行は残す。
    pub fn evict_far(&mut self, keep: std::ops::Range<usize>) {
        let pinned: Vec<usize> = self
            .cursors
            .iter()
            .flat_map(|sel| [sel.start().line, sel.end().line])
            .collect();
        self.text.evict_far(keep, &pinned);
    }

    /// 届いた行を `from` から順に入れます。既にある行はそのまま。
    pub fn feed(&mut self, from: usize, lines: Vec<crate::structure::text::SourceLine>) {
        for (offset, line) in lines.into_iter().enumerate() {
            self.text.fill_line(from + offset, line);
        }
    }

    /// 選択のいずれかが、まだ届いていない行に触れているか。届く前の行は
    /// 空に見えているだけなので、そこへの編集は中身を黙って壊してしまう。
    fn touches_absent(&self) -> bool {
        // 全部届いた文書では行を見に行かない。キー入力のたびに全行を
        // 走査すると、大きい文書の入力が重くなる。
        if self.text.absent_lines() == 0 {
            return false;
        }
        self.cursors.iter().any(|sel| {
            self.text
                .first_absent(sel.start().line)
                .is_some_and(|absent| absent <= sel.end().line)
        })
    }

    fn edit_each(&mut self, step: Step, edit: impl Fn(&Text, Sel) -> (Pos, Pos, Vec<Row>)) {
        let order: Vec<usize> = self
            .cursors
            .iter()
            .enumerate()
            .filter_map(|(index, cursor)| cursor.inside.is_none().then_some(index))
            .collect();
        self.edit_indices(step, order, edit);
    }

    fn edit_indices(
        &mut self,
        step: Step,
        mut order: Vec<usize>,
        edit: impl Fn(&Text, Sel) -> (Pos, Pos, Vec<Row>),
    ) {
        if order.is_empty() {
            return;
        }
        self.record(step);
        order.sort_by_key(|&i| self.cursors[i].start());
        for (done, &i) in order.iter().enumerate() {
            let (from, to, what) = edit(&self.text, self.cursors[i].sel);
            let at = self.text.remove(from, to);
            let end = self.text.insert(at, what);
            self.cursors[i] = UnifiedCursor::caret(end);
            for &later in &order[done + 1..] {
                let sel = self.cursors[later].sel;
                self.cursors[later] =
                    UnifiedCursor::range(shifted(sel.anchor, to, end), shifted(sel.head, to, end));
            }
        }
        self.merge_sels();
    }

    pub fn insert(&mut self, what: Vec<Row>) {
        let indices: Vec<usize> = self
            .cursors
            .iter()
            .enumerate()
            .filter_map(|(index, cursor)| cursor.inside.is_none().then_some(index))
            .collect();
        self.insert_indices(what, indices);
    }

    fn insert_indices(&mut self, what: Vec<Row>, indices: Vec<usize>) {
        if self.touches_absent() {
            return;
        }
        let typing = what.len() == 1 && what[0].len() == 1;
        let step = if typing { Step::Typing } else { Step::Other };
        self.edit_indices(step, indices, move |_, sel| {
            (sel.start(), sel.end(), what.clone())
        });
    }

    /// キャレットがどこにあっても、そのキャレットにテキストを挿入します。単一の文字が入力されるため、構造内のショートカットは引き続き実行されます。それ以上のものはペーストなのでそのまま入ります。
    pub fn insert_text(&mut self, text: &str) -> Did {
        if self.touches_absent() {
            return Did::Nothing;
        }
        let top_level: Vec<usize> = self
            .cursors
            .iter()
            .enumerate()
            .filter_map(|(index, cursor)| cursor.inside.is_none().then_some(index))
            .collect();
        if self.has_inside() {
            let mut chars = text.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => {
                    self.type_with_cursor(c);
                }
                // 文字は文字のままです。貼り付けでは、文字を入力したときのショートカットが再実行されることはありません。構造体は 1 行を保持するため、その内部では改行は何の意味も持ちません。
                _ => {
                    self.insert_nested_row(
                        text.chars()
                            .filter(|c| *c != '\n')
                            .map(Node::char)
                            .collect(),
                    );
                }
            };
        }
        self.insert_indices(nodes_of(text), top_level);
        Did::Changed
    }

    /// ドキュメントからコピーされた部分を、元の形状のまま元に戻します。他の場所からのテキストは、[`Self::insert_text`] を介して文字として到着します。
    pub fn insert_clip(&mut self, clip: &Clip) -> Did {
        if self.touches_absent() {
            return Did::Nothing;
        }
        if self.has_inside() {
            self.insert_nested_row(clip.row());
        } else {
            self.insert(clip.lines());
        }
        Did::Changed
    }

    pub fn annotate(&mut self, upper: bool) -> Did {
        if self.touches_absent() {
            return Did::Nothing;
        }
        if self.has_inside() {
            let mut annotated = false;
            let changed = self.with_each_cursor(Inside::Change, |editing| {
                annotated |= editing.annotate(upper);
                None
            });
            if changed && annotated && self.cursors.iter().all(|cursor| cursor.inside.is_some()) {
                return Did::Changed;
            }
        }
        let sel = self.primary();
        if sel.is_caret() && self.enter_node_beside(false) {
            return self.annotate(upper);
        }
        if sel.is_caret() && self.enter_node_beside(true) {
            return self.annotate(upper);
        }
        if !sel.is_caret() && sel.start().line == sel.end().line {
            let lines = self.text.slice(sel.start(), sel.end());
            let Some(items) = lines.first() else {
                return Did::Nothing;
            };
            if items
                .iter()
                .any(|node| matches!(node.kind, crate::structure::ast::NodeKind::Tab))
            {
                return Did::Nothing;
            }
            let base = items.clone();
            if base.is_empty() {
                return Did::Nothing;
            }
            let node = Node::container(base);
            let slot = if upper {
                node.upper_slot()
            } else {
                node.lower_slot()
            };
            let at = sel.start();
            self.replace_range_with(at, sel.end(), vec![vec![node]]);
            self.cursors = vec![UnifiedCursor {
                sel: Sel::caret(at),
                inside: Some(Cursor {
                    path: vec![(at.col, slot)],
                    index: 0,
                    anchor: 0,
                    fills: Vec::new(),
                }),
                transient_structure: None,
            }];
            return Did::Changed;
        }
        Did::Nothing
    }

    /// 本文では列区切りを挿入し、入れ子構造では次のスロットへ移動します。
    pub fn tab(&mut self, back: bool) -> Did {
        if self.touches_absent() {
            return Did::Nothing;
        }
        let nested = self.cursors.iter().any(|selection| {
            selection
                .inside
                .as_ref()
                .is_some_and(|cursor| !cursor.path.is_empty())
        });
        if nested {
            self.with_each_cursor(Inside::Move, |editing| {
                if back {
                    editing.move_left()
                } else {
                    editing.move_right()
                }
            });
        }
        self.insert(vec![vec![Node::tab()]]);
        if nested && self.cursors.iter().all(|cursor| cursor.inside.is_some()) {
            Did::Moved
        } else {
            Did::Changed
        }
    }

    /// 本文では行を分割し、入れ子構造の編集中なら改行せず構造を抜けます。
    pub fn split_line(&mut self) -> Did {
        if self.touches_absent() {
            return Did::Nothing;
        }
        let top_level: Vec<usize> = self
            .cursors
            .iter()
            .enumerate()
            .filter_map(|(index, cursor)| cursor.inside.is_none().then_some(index))
            .collect();
        let left = self.leave_structure();
        if !top_level.is_empty() {
            self.insert_indices(vec![Vec::new(), Vec::new()], top_level);
            Did::Changed
        } else if left {
            Did::Moved
        } else {
            Did::Nothing
        }
    }

    /// 入れ子構造の編集を終了するか、余分なカーソルを削除します。
    pub fn escape(&mut self) -> Did {
        Did::moved(self.leave_structure() || self.collapse_sels())
    }

    pub fn backspace(&mut self) -> Did {
        if self.touches_absent() {
            return Did::Nothing;
        }
        if self.has_inside() {
            self.with_each_cursor(Inside::Change, |editing| editing.backspace());
        }
        self.backspace_in_text();
        Did::Changed
    }

    fn backspace_in_text(&mut self) {
        self.edit_each(Step::Other, |text, sel| {
            if sel.is_caret() {
                (before(text, sel.head), sel.head, Vec::new())
            } else {
                (sel.start(), sel.end(), Vec::new())
            }
        });
    }

    pub fn delete_forward(&mut self) -> Did {
        if self.touches_absent() {
            return Did::Nothing;
        }
        if self.has_inside() {
            self.with_each_cursor(Inside::Change, |editing| {
                editing.delete_forward();
                None
            });
        }
        self.edit_each(Step::Other, |text, sel| {
            if sel.is_caret() {
                (sel.head, after(text, sel.head), Vec::new())
            } else {
                (sel.start(), sel.end(), Vec::new())
            }
        });
        Did::Changed
    }

    /// ケアトのグリッドは、構造内のものだけを意味し、列によって成長します。
    pub fn grow_matrix(&mut self) -> Did {
        if self.touches_absent() {
            return Did::Nothing;
        }
        if !self.has_inside() {
            return Did::Nothing;
        }
        self.with_cursor(Inside::Change, |editing| {
            editing.grow_matrix(true);
            None
        });
        Did::Changed
    }

    /// 検索と置換によって使用される1つの範囲を置換します。
    pub fn replace_range(&mut self, from: Pos, to: Pos, with: &str) {
        self.replace_range_with(from, to, nodes_of(with));
    }

    /// カラムの区切り文字よりも多くの文字を入れる置換のために、アイテムと範囲を置換します。
    pub fn replace_range_with(&mut self, from: Pos, to: Pos, with: Vec<Row>) {
        if self
            .text
            .first_absent(from.line)
            .is_some_and(|absent| absent <= to.line)
        {
            return;
        }
        self.record(Step::Other);
        self.clear_inside();
        let at = self.text.remove(from, to);
        let end = self.text.insert(at, with);
        self.cursors = vec![UnifiedCursor::caret(end)];
    }

    /// 左右へ1つ移動し、構造Nodeでは外側を飛び越えず編集可能なスロットへ入ります。
    pub fn move_h(&mut self, forward: bool, extend: bool) -> Did {
        if self.has_inside() {
            let kind = if extend { Inside::Extend } else { Inside::Move };
            self.with_each_cursor(kind, |editing| {
                if extend {
                    editing.extend(forward)
                } else if forward {
                    editing.move_right()
                } else {
                    editing.move_left()
                }
            });
        }
        if !self.has_inside() && !extend && self.enter_node_beside(forward) {
            return Did::Moved;
        }
        self.map_sels(extend, |text, head| {
            if forward {
                after(text, head)
            } else {
                before(text, head)
            }
        });
        Did::Moved
    }

    /// 本文では行間を移動し、入れ子構造では内容のある上下スロット間を移動します。
    pub fn move_v(&mut self, down: bool, extend: bool) -> Did {
        if self.has_inside() {
            self.move_vertical_cursors(down);
        }
        self.map_sels(extend, |text, head| {
            let line = if down {
                head.line + 1
            } else {
                head.line.checked_sub(1).unwrap_or(head.line)
            };
            text.clamp(Pos::new(line.min(text.line_count() - 1), head.col))
        });
        Did::Moved
    }

    pub fn move_line_edge(&mut self, end: bool, extend: bool) -> Did {
        if self.has_inside() {
            self.with_each_cursor(Inside::Move, |editing| {
                if end {
                    editing.move_end();
                } else {
                    editing.move_home();
                }
                None
            });
        }
        self.map_sels(extend, |text, head| {
            Pos::new(head.line, if end { text.line_len(head.line) } else { 0 })
        });
        Did::Moved
    }

    pub fn move_document_edge(&mut self, end: bool, extend: bool) -> Did {
        self.leave_structure();
        self.map_sels(
            extend,
            |text, _| {
                if end {
                    text.end()
                } else {
                    Pos::default()
                }
            },
        );
        Did::Moved
    }

    fn map_sels(&mut self, extend: bool, step: impl Fn(&Text, Pos) -> Pos) {
        self.recorder.cut();
        for sel in self
            .cursors
            .iter_mut()
            .filter(|cursor| cursor.inside.is_none())
        {
            // 他のエディタと同様に、Shift を使用せずに選択範囲を折りたたむと、近くの端が維持されます。
            let from = if extend || sel.is_caret() {
                sel.head
            } else {
                sel.head.min(sel.anchor).max(sel.start())
            };
            let head = step(&self.text, from);
            sel.head = head;
            if !extend {
                sel.anchor = head;
            }
        }
        self.merge_sels();
    }

    pub fn set_caret(&mut self, at: Pos) {
        self.recorder.cut();
        self.clear_inside();
        self.cursors = vec![UnifiedCursor::caret(self.text.clamp(at))];
    }

    pub fn extend_to(&mut self, at: Pos) {
        self.recorder.cut();
        self.clear_inside();
        let at = self.text.clamp(at);
        if let Some(sel) = self.cursors.last_mut() {
            sel.head = at;
        }
        self.merge_sels();
    }

    pub fn add_caret(&mut self, at: Pos) {
        self.recorder.cut();
        self.clear_inside();
        self.cursors.push(UnifiedCursor::caret(self.text.clamp(at)));
        self.merge_sels();
    }

    /// キャレットが存在する場所を選択するために存在するものすべてを選択します (キャレットが含まれる構造の行、またはドキュメント全体)。
    pub fn select_all(&mut self) -> Did {
        if self.has_inside() {
            self.with_cursor(Inside::Extend, |editing| {
                editing.select_row();
                None
            });
            return Did::Moved;
        }
        self.recorder.cut();
        self.cursors = vec![UnifiedCursor::range(Pos::default(), self.text.end())];
        Did::Moved
    }

    pub fn set_sels(&mut self, sels: Vec<Sel>) {
        self.recorder.cut();
        self.clear_inside();
        if sels.is_empty() {
            return;
        }
        self.cursors = sels
            .into_iter()
            .map(|sel| UnifiedCursor {
                sel,
                inside: None,
                transient_structure: None,
            })
            .collect();
        self.merge_sels();
    }

    /// 余分なカーソルを削除し、フォーカスのあるカーソルを保持します。
    pub fn collapse_sels(&mut self) -> bool {
        if self.cursors.len() == 1 {
            return false;
        }
        let primary = self.primary_cursor().clone();
        self.cursors = vec![primary];
        true
    }

    /// `Ctrl+D`: キャレットの単語を選択し、さらに押すたびに、同じテキストが表示される次の場所が追加されます。
    pub fn add_next_occurrence(&mut self) -> bool {
        // 構造体は 1 つのキャレットを保持するため、そこに追加するものは何もありません。
        if self.has_inside() {
            return false;
        }
        self.recorder.cut();
        let primary = self.primary();
        if primary.is_caret() {
            let Some(word) = word_at(&self.text, primary.head) else {
                return false;
            };
            self.cursors.last_mut().expect("a selection").sel = word;
            return true;
        }
        let needle: Row = self
            .text
            .slice(primary.start(), primary.end())
            .into_iter()
            .next()
            .unwrap_or_default();
        if needle.is_empty() || primary.start().line != primary.end().line {
            return false;
        }
        let taken: Vec<Pos> = self.cursors.iter().map(|cursor| cursor.start()).collect();
        let Some(found) = find_after(&self.text, &needle, primary.end(), &taken) else {
            return false;
        };
        self.cursors.push(UnifiedCursor {
            sel: found,
            inside: None,
            transient_structure: None,
        });
        true
    }

    /// 選択内容がソートされ、重複がない状態が維持されるため、入力によって同じ編集が 2 回適用されることはありません。
    fn merge_sels(&mut self) {
        let primary = self.primary_cursor().clone();
        self.cursors.sort_by_key(|cursor| {
            (
                cursor.start(),
                cursor.inside.as_ref().map(|inside| inside.path.clone()),
                cursor.end(),
            )
        });
        let mut merged: Vec<UnifiedCursor> = Vec::with_capacity(self.cursors.len());
        for cursor in std::mem::take(&mut self.cursors) {
            match merged.last_mut() {
                Some(last)
                    if last.inside.is_none()
                        && cursor.inside.is_none()
                        && cursor.start() <= last.end() =>
                {
                    if cursor.end() > last.end() {
                        last.sel = Sel::range(last.start(), cursor.end());
                    }
                }
                Some(last) if *last == cursor => {}
                _ => merged.push(cursor),
            }
        }
        // 「プライマリ」がそれを意味し続けるように、フォーカスされた選択範囲は最後に残す。
        if let Some(index) = merged.iter().position(|cursor| {
            *cursor == primary
                || (cursor.inside.is_none()
                    && primary.inside.is_none()
                    && cursor.start() <= primary.start()
                    && primary.end() <= cursor.end())
        }) {
            let focused = merged.remove(index);
            merged.push(focused);
        }
        self.cursors = merged;
    }
}

fn before(text: &Text, at: Pos) -> Pos {
    if at.col > 0 {
        Pos::new(at.line, at.col - 1)
    } else if at.line > 0 {
        Pos::new(at.line - 1, text.line_len(at.line - 1))
    } else {
        at
    }
}

fn after(text: &Text, at: Pos) -> Pos {
    if at.col < text.line_len(at.line) {
        Pos::new(at.line, at.col + 1)
    } else if at.line + 1 < text.line_count() {
        Pos::new(at.line + 1, 0)
    } else {
        at
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn word_at(text: &Text, at: Pos) -> Option<Sel> {
    let line = text.line(at.line);
    let word = |col: usize| line.get(col).and_then(as_char).is_some_and(is_word);
    let mut start = at.col;
    while start > 0 && word(start - 1) {
        start -= 1;
    }
    let mut end = at.col;
    while word(end) {
        end += 1;
    }
    (start < end).then(|| Sel::range(Pos::new(at.line, start), Pos::new(at.line, end)))
}

/// 既に選択した場所をスキップして、 `from` からアイテムを探します。
fn find_after(text: &Text, needle: &[Node], from: Pos, taken: &[Pos]) -> Option<Sel> {
    let mut at = from;
    for _ in 0..text.line_count() + 1 {
        for line in at.line..text.line_count() {
            let items = text.line(line);
            let start_col = if line == at.line { at.col } else { 0 };
            for col in start_col..=items.len().saturating_sub(needle.len()) {
                if items.len() < needle.len() {
                    break;
                }
                if &items[col..col + needle.len()] == needle
                    && !taken.contains(&Pos::new(line, col))
                {
                    return Some(Sel::range(
                        Pos::new(line, col),
                        Pos::new(line, col + needle.len()),
                    ));
                }
            }
        }
        if at == Pos::default() {
            return None;
        }
        at = Pos::default();
    }
    None
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    /// ここでは、表記を通過するものは何もありません。モデルは渡されるアイテムのみであるため、ファイル形式によってはこれらのテストを開始できません。
    pub(crate) fn editor(source: &str) -> Editor {
        with_rows(nodes_of(source))
    }

    pub(crate) fn with_rows(lines: Vec<Row>) -> Editor {
        let mut editor = Editor::default();
        editor.load(Text::from_lines(lines));
        editor
    }

    pub(crate) fn plain(editor: &Editor) -> String {
        let text = editor.text();
        let rows = text.slice(Pos::default(), text.end());
        crate::structure::plain::lines(&rows)
    }

    #[test]
    fn edits_touching_lines_that_have_not_arrived_do_nothing() {
        use crate::structure::text::SourceLine;
        let mut editor = Editor::default();
        editor.load_pending(3);
        editor.insert_text("x");
        editor.split_line();
        editor.backspace();
        assert_eq!(editor.text().line_count(), 3);
        assert_eq!(editor.text().line_len(0), 0);
        editor.feed(0, vec![SourceLine::Plain("ab".into())]);
        // 届いた行は編集できる。まだの行はそのまま。
        editor.set_caret(Pos::new(0, 2));
        editor.insert_text("X");
        editor.feed(
            1,
            vec![
                SourceLine::Plain("cd".into()),
                SourceLine::Plain("ef".into()),
            ],
        );
        assert_eq!(plain(&editor), "abX\ncd\nef");
    }

    #[test]
    fn feeding_fills_only_lines_that_have_not_arrived() {
        use crate::structure::text::SourceLine;
        let mut editor = Editor::default();
        editor.load_pending(2);
        editor.feed(1, vec![SourceLine::Plain("late".into())]);
        assert_eq!(editor.text().first_absent(0), Some(0));
        editor.feed(0, vec![SourceLine::Plain("first".into())]);
        assert_eq!(editor.text().first_absent(0), None);
        assert_eq!(plain(&editor), "first\nlate");
    }

    /// 履歴の 1 ステップは文書の本体が持つ。ここではグループ番号の付き方
    /// （入力の結合、1 操作へのまとめ）と、控えの回収を確かめる。
    #[test]
    fn typed_characters_share_one_group_and_other_edits_start_new_ones() {
        let mut editor = editor("ab");
        editor.set_caret(Pos::new(0, 1));
        editor.insert_text("X");
        let first = editor.take_flush().expect("typing changed the text");
        editor.insert_text("Y");
        let second = editor.take_flush().expect("typing changed the text");
        assert_eq!(first.group, second.group);
        assert_eq!(second.before, "");
        editor.set_caret(Pos::new(0, 0));
        editor.insert_text("Z");
        let third = editor.take_flush().expect("typing changed the text");
        assert_ne!(second.group, third.group);
        // 控えは編集直前のキャレット。元に戻すとここへ帰る。
        assert_eq!(third.before, "0.0-0.0");
    }

    #[test]
    fn one_step_keeps_every_edit_in_one_group() {
        let mut editor = editor("ab");
        editor.set_caret(Pos::new(0, 2));
        editor.one_step(|editor| {
            editor.insert_text("X");
            editor.split_line();
            editor.insert_text("Y");
        });
        let flush = editor.take_flush().expect("the step changed the text");
        assert_eq!(flush.changes.len(), 1);
        editor.insert_text("Z");
        let next = editor.take_flush().expect("typing changed the text");
        assert_ne!(flush.group, next.group);
    }

    #[test]
    fn restoring_forgets_local_lines_and_puts_the_caret_back() {
        let mut editor = editor("ab\ncd");
        editor.set_caret(Pos::new(1, 2));
        editor.insert_text("X");
        editor.take_flush();
        editor.apply_restored("1.2-1.2", 1, 2);
        assert_eq!(editor.primary().head, Pos::new(1, 0));
        assert_eq!(editor.text().first_absent(0), Some(1));
        assert_eq!(editor.take_flush().map(|flush| flush.changes), None);
    }

    #[test]
    fn a_separator_is_one_node() {
        let mut editor = editor("x= 1");
        editor.set_caret(Pos::new(0, 1));
        editor.tab(false);
        assert!(matches!(
            editor.text().node_at(Pos::new(0, 1)).map(|node| &node.kind),
            Some(crate::structure::ast::NodeKind::Tab)
        ));
        assert_eq!(editor.text().line_len(0), 5);
    }

    #[test]
    fn typing_at_two_cursors_edits_both() {
        let mut editor = editor("ab\nab");
        editor.set_caret(Pos::new(0, 1));
        editor.add_caret(Pos::new(1, 1));
        editor.insert_text("X");
        assert_eq!(plain(&editor), "aXb\naXb");
        assert_eq!(editor.sels().len(), 2);
        assert_eq!(editor.sels()[0].head, Pos::new(0, 2));
    }

    #[test]
    fn cursors_on_one_line_stay_on_their_own_text() {
        let mut editor = editor("AAA BBB");
        editor.set_caret(Pos::new(0, 3));
        editor.add_caret(Pos::new(0, 7));
        editor.insert_text("X");
        assert_eq!(plain(&editor), "AAAX BBBX");
        // 各キャレットは、入力したばかりの文字を削除する必要があります。
        editor.backspace();
        assert_eq!(plain(&editor), "AAA BBB");
    }

    #[test]
    fn a_newline_at_an_earlier_cursor_moves_the_later_one() {
        let mut editor = editor("ab cd");
        editor.set_caret(Pos::new(0, 2));
        editor.add_caret(Pos::new(0, 5));
        editor.split_line();
        assert_eq!(plain(&editor), "ab\n cd\n");
        assert_eq!(editor.sels()[1].head, Pos::new(2, 0));
    }

    #[test]
    fn nested_cursor_state_roundtrips_for_history() {
        let mut editor = editor("ab\ncd");
        editor.set_caret(Pos::new(0, 1));
        editor.add_caret(Pos::new(1, 1));
        editor.start_structure();
        editor.cursors[0].inside.as_mut().unwrap().fills = vec![2, 3];
        editor.cursors[0].transient_structure = Some(1);
        let state = editor.state_string();
        let expected = editor.cursors.clone();
        editor.set_caret(Pos::default());
        editor.restore_state(&state);
        assert_eq!(editor.cursors, expected);
    }

    #[test]
    fn multiple_root_cursors_type_inside_structure_mode() {
        let mut editor = editor("a\nb");
        editor.set_caret(Pos::new(0, 1));
        editor.add_caret(Pos::new(1, 1));
        editor.start_structure();
        editor.insert_text("X");
        assert_eq!(plain(&editor), "aX\nbX");
        assert_eq!(editor.cursors().len(), 2);
        assert!(editor
            .cursors()
            .iter()
            .all(|cursor| cursor.inside.is_some()));
    }

    #[test]
    fn multiple_root_cursors_apply_fraction_trigger() {
        let mut editor = editor("a\nb");
        editor.set_caret(Pos::new(0, 1));
        editor.add_caret(Pos::new(1, 1));
        editor.start_structure();
        editor.insert_text("/");
        assert_eq!(plain(&editor), "a/\nb/");
        assert!(editor
            .cursors()
            .iter()
            .all(|cursor| cursor.inside.is_some()));
    }

    #[test]
    fn multiple_root_cursors_transform_two_places_on_one_line() {
        let mut editor = editor("a b");
        editor.set_caret(Pos::new(0, 1));
        editor.add_caret(Pos::new(0, 3));
        editor.start_structure();
        editor.insert_text("/");
        let row = editor.text().line(0);
        assert!(matches!(
            row[0].kind,
            crate::structure::ast::NodeKind::Stack { .. }
        ));
        assert!(matches!(
            row[2].kind,
            crate::structure::ast::NodeKind::Stack { .. }
        ));
        editor.insert_text("X");
        for cursor in editor.cursors() {
            let inside = cursor.inside.as_ref().expect("fraction slot");
            assert_eq!(
                crate::structure::ast::row_at(editor.text().line(0), &inside.path),
                Some([Node::char('X')].as_slice())
            );
        }
    }

    #[test]
    fn a_multi_cursor_edit_is_one_group_of_line_changes() {
        let mut editor = editor("ab\nab");
        editor.set_caret(Pos::new(0, 1));
        editor.add_caret(Pos::new(1, 1));
        editor.insert_text("X");
        editor.insert_text("Y");
        assert_eq!(plain(&editor), "aXYb\naXYb");
        // 両方のキャレットの編集と続けた入力が、履歴の 1 ステップに入る。
        let flush = editor.take_flush().expect("typing changed the text");
        assert_eq!(flush.changes.len(), 2);
        assert_eq!(flush.changes[0].from, 0);
        assert_eq!(flush.changes[1].from, 1);
    }

    #[test]
    fn backspace_joins_lines_and_deletes_a_structure_node_whole() {
        let structure = Node::sqrt(None, vec![Node::char('x')]);
        let mut editor = with_rows(vec![
            vec![Node::char('a'), structure],
            vec![Node::char('b')],
        ]);
        editor.set_caret(Pos::new(1, 0));
        editor.backspace();
        assert_eq!(plain(&editor), "a√xb");
        editor.set_caret(Pos::new(0, 2));
        editor.backspace();
        assert_eq!(plain(&editor), "ab");
    }

    #[test]
    fn enter_splits_and_reports_the_line_change() {
        let mut editor = editor("ab");
        editor.set_caret(Pos::new(0, 1));
        editor.split_line();
        assert_eq!(plain(&editor), "a\nb");
        assert_eq!(editor.primary().head, Pos::new(1, 0));
        let flush = editor.take_flush().expect("the split changed the text");
        assert_eq!(
            flush.changes,
            vec![crate::structure::text::LineChange {
                from: 0,
                removed: 1,
                inserted: 2
            }]
        );
    }

    #[test]
    fn ctrl_d_selects_the_word_then_the_next_one() {
        let mut editor = editor("foo bar\nfoo");
        editor.set_caret(Pos::new(0, 1));
        assert!(editor.add_next_occurrence());
        assert_eq!(editor.primary(), Sel::range(Pos::new(0, 0), Pos::new(0, 3)));
        assert!(editor.add_next_occurrence());
        assert_eq!(editor.sels().len(), 2);
        assert_eq!(editor.primary(), Sel::range(Pos::new(1, 0), Pos::new(1, 3)));
        editor.insert_text("qux");
        assert_eq!(plain(&editor), "qux bar\nqux");
    }

    #[test]
    fn overlapping_cursors_collapse_into_one() {
        let mut editor = editor("abc");
        editor.set_caret(Pos::new(0, 1));
        editor.add_caret(Pos::new(0, 1));
        assert_eq!(editor.sels().len(), 1);
    }

    #[test]
    fn moving_left_from_a_big_operator_enters_its_last_nonempty_annotation() {
        let mut operator = Node::big_op("∑".into());
        operator.lower = vec![Node::char('i')];
        operator.upper = vec![Node::char('n')];
        let upper = operator.upper_slot();
        let mut editor = with_rows(vec![vec![operator]]);
        editor.set_caret(Pos::new(0, 1));
        editor.move_h(false, false);
        assert_eq!(
            editor.nested_cursor().map(|cursor| cursor.path.clone()),
            Some(vec![(0, upper)])
        );
    }

    #[test]
    fn moving_down_keeps_the_column_within_the_line() {
        let mut editor = editor("long line\nab");
        editor.set_caret(Pos::new(0, 9));
        editor.move_v(true, false);
        assert_eq!(editor.primary().head, Pos::new(1, 2));
    }

    #[test]
    fn vertical_movement_skips_empty_annotations_and_continues_to_the_next_line() {
        let operator = Node::big_op("∑".into());
        let lower = operator.lower_slot();
        let mut editor = with_rows(vec![
            vec![Node::char('a')],
            vec![Node::char('b'), operator],
            vec![Node::char('c'), Node::char('d')],
        ]);
        let at = Pos::new(1, 1);
        assert!(editor.enter_at(
            at,
            &Cursor {
                path: vec![(1, lower)],
                index: 0,
                anchor: 0,
                fills: Vec::new(),
            },
        ));

        editor.move_v(true, false);

        assert!(editor.nested_cursor().is_none());
        assert_eq!(editor.primary().head, Pos::new(2, 1));
    }

    #[test]
    fn vertical_movement_enters_a_nonempty_annotation() {
        let mut operator = Node::big_op("∑".into());
        operator.upper = vec![Node::char('n')];
        let upper = operator.upper_slot();
        let mut editor = with_rows(vec![
            vec![Node::char('a')],
            vec![Node::char('b'), operator],
            vec![Node::char('c')],
        ]);
        let at = Pos::new(1, 1);
        assert!(editor.enter_at(at, &Cursor::root(2)));

        editor.move_v(false, false);

        assert_eq!(editor.primary().head, at);
        assert_eq!(
            editor.nested_cursor().expect("inside annotation").path,
            vec![(1, upper)]
        );
    }

    #[test]
    fn moving_from_a_top_level_annotation_returns_to_its_text() {
        let mut editor = editor("abc");
        editor.set_sels(vec![Sel::range(Pos::new(0, 0), Pos::new(0, 3))]);
        assert!(matches!(editor.annotate(true), Did::Changed));
        editor.insert_text("n");
        editor.move_v(true, false);
        assert_eq!(
            editor.nested_cursor().expect("inside base").path,
            vec![(0, 0)]
        );
        editor.insert_text("X");
        assert_eq!(
            crate::structure::plain::row(&editor.text().line(0).to_vec()),
            "aXbc^n"
        );
    }

    #[test]
    fn selecting_then_typing_replaces_the_selection() {
        let mut editor = editor("hello");
        editor.set_caret(Pos::new(0, 0));
        editor.extend_to(Pos::new(0, 5));
        editor.insert_text("bye");
        assert_eq!(plain(&editor), "bye");
    }
}
