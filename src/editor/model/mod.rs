//! ドキュメントの編集: 選択内容、適用されるコマンド、および元に戻す履歴。
//!
//! すべてのコマンドは、複数のカーソルが予期される動作として、単一のステップとしてすべての選択に適用されます。ドキュメント自体は [`crate::structural::text`] であり、表記や画面については何も知りません。

mod history;
mod island;

use crate::structure::edit::Escape;
use history::{History, Step};
pub use island::Inside;

use super::clipboard::Clip;
use crate::structure::ast::{Cursor, Node, Row};
use crate::structure::text::{items_of, Item, Pos, Sel, Text};

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

pub struct Editor {
    text: Text,
    sels: Vec<Sel>,
    /// ケアトが島の中にいるところ、それが1つにいるとき。 注意は、いずれかの文書の1つの場所にある: これは、それが到達するその場所の深さだけを言う.
    inside: Option<Cursor>,
    history: History,
}

impl Default for Editor {
    fn default() -> Self {
        Self {
            text: Text::default(),
            sels: vec![Sel::caret(Pos::default())],
            inside: None,
            history: History::default(),
        }
    }
}

impl Editor {
    pub fn text(&self) -> &Text {
        &self.text
    }

    pub fn sels(&self) -> &[Sel] {
        &self.sels
    }

    /// 焦点を維持する選択。最後に追加したもの。
    pub fn primary(&self) -> Sel {
        *self.sels.last().expect("at least one selection")
    }

    /// ファイルから読み取られたばかりのドキュメントを表示し、履歴を削除します。
    pub fn load(&mut self, text: Text) {
        self.text = text;
        self.sels = vec![Sel::caret(Pos::default())];
        self.inside = None;
        self.history.clear();
    }

    fn edit_each(&mut self, step: Step, edit: impl Fn(&Text, Sel) -> (Pos, Pos, Vec<Vec<Item>>)) {
        self.record(step);
        self.inside = None;
        let mut order: Vec<usize> = (0..self.sels.len()).collect();
        order.sort_by_key(|&i| self.sels[i].start());
        for (done, &i) in order.iter().enumerate() {
            let (from, to, what) = edit(&self.text, self.sels[i]);
            let at = self.text.remove(from, to);
            let end = self.text.insert(at, what);
            self.sels[i] = Sel::caret(end);
            for &later in &order[done + 1..] {
                let sel = self.sels[later];
                self.sels[later] =
                    Sel::range(shifted(sel.anchor, to, end), shifted(sel.head, to, end));
            }
        }
        self.merge_sels();
    }

    pub fn insert(&mut self, what: Vec<Vec<Item>>) {
        let typing = what.len() == 1 && what[0].len() == 1;
        let step = if typing { Step::Typing } else { Step::Other };
        self.edit_each(step, move |_, sel| (sel.start(), sel.end(), what.clone()));
    }

    /// キャレットがどこにあっても、そのキャレットにテキストを挿入します。単一の文字が入力されるため、構造内のショートカットは引き続き実行されます。それ以上のものはペーストなのでそのまま入ります。
    pub fn insert_text(&mut self, text: &str) -> Did {
        if self.inside.is_some() {
            let mut chars = text.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => self.type_in_island(c),
                // 文字は文字のままです。貼り付けでは、文字を入力したときのショートカットが再実行されることはありません。構造体は 1 行を保持するため、その内部では改行は何の意味も持ちません。
                _ => self.insert_row_in_island(
                    text.chars()
                        .filter(|c| *c != '\n')
                        .map(Node::Char)
                        .collect(),
                ),
            };
            return Did::Changed;
        }
        self.insert(items_of(text));
        Did::Changed
    }

    /// ドキュメントからコピーされた部分を、元の形状のまま元に戻します。他の場所からのテキストは、[`Self::insert_text`] を介して文字として到着します。
    pub fn insert_clip(&mut self, clip: &Clip) -> Did {
        if self.inside.is_some() {
            self.insert_row_in_island(clip.row());
        } else {
            self.insert(clip.items());
        }
        Did::Changed
    }

    pub fn insert_math(&mut self, row: Row) {
        self.insert(vec![vec![Item::Math(row)]]);
    }

    /// タブ: テキスト内の列の区切り文字、および構造内の次のスロットへのステップ。これがすべての数式エディターでのタブの意味です。
    pub fn tab(&mut self, back: bool) -> Did {
        if self.inside.is_some() {
            self.in_island(Inside::Move, |editing| {
                if back {
                    editing.move_left()
                } else {
                    editing.move_right()
                }
            });
            return Did::Moved;
        }
        self.insert(vec![vec![Item::Tab]]);
        Did::Changed
    }

    /// テキストに新しい行を入力し、その中に式の終わりを入力します。
    pub fn split_line(&mut self) -> Did {
        if self.leave_island() {
            return Did::Moved;
        }
        self.insert(vec![Vec::new(), Vec::new()]);
        Did::Changed
    }

    /// エスケープ: 式を終了するか、余分なカーソルを削除します。
    pub fn escape(&mut self) -> Did {
        Did::moved(self.leave_island() || self.collapse_sels())
    }

    pub fn backspace(&mut self) -> Did {
        if self.inside.is_some() {
            self.in_island(Inside::Change, |editing| editing.backspace());
            return Did::Changed;
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
        if self.inside.is_some() {
            self.in_island(Inside::Change, |editing| {
                editing.delete_forward();
                None
            });
            return Did::Changed;
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
        if self.inside.is_none() {
            return Did::Nothing;
        }
        self.in_island(Inside::Change, |editing| {
            editing.grow_matrix(true);
            None
        });
        Did::Changed
    }

    /// 検索と置換によって使用される1つの範囲を置換します。
    pub fn replace_range(&mut self, from: Pos, to: Pos, with: &str) {
        self.replace_range_with(from, to, items_of(with));
    }

    /// カラムの区切り文字よりも多くの文字を入れる置換のために、アイテムと範囲を置換します。
    pub fn replace_range_with(&mut self, from: Pos, to: Pos, with: Vec<Vec<Item>>) {
        self.record(Step::Other);
        self.inside = None;
        let at = self.text.remove(from, to);
        let end = self.text.insert(at, with);
        self.sels = vec![Sel::caret(end)];
    }

    /// 左と右に、一度に1つの場所。 数式は、キャレットが上回るのではなく、内部の1つの場所は構造自体です。
    pub fn move_h(&mut self, forward: bool, extend: bool) -> Did {
        if self.inside.is_some() {
            let kind = if extend { Inside::Extend } else { Inside::Move };
            self.in_island(kind, |editing| {
                if extend {
                    editing.extend(forward)
                } else if forward {
                    editing.move_right()
                } else {
                    editing.move_left()
                }
            });
            return Did::Moved;
        }
        if !extend && self.enter_island_beside(forward) {
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

    /// 上下: テキストの行間と、キャレットが入っている構造のスロット間。数式の上部または下部を残すと、キャレットがテキストに戻ります。
    pub fn move_v(&mut self, down: bool, extend: bool) -> Did {
        if self.inside.is_some() {
            self.in_island(Inside::Move, |editing| {
                let moved = if down {
                    editing.move_down()
                } else {
                    editing.move_up()
                };
                (!moved).then_some(Escape::Done)
            });
            return Did::Moved;
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
        if self.inside.is_some() {
            self.in_island(Inside::Move, |editing| {
                if end {
                    editing.move_end();
                } else {
                    editing.move_home();
                }
                None
            });
            return Did::Moved;
        }
        self.map_sels(extend, |text, head| {
            Pos::new(head.line, if end { text.line_len(head.line) } else { 0 })
        });
        Did::Moved
    }

    pub fn move_document_edge(&mut self, end: bool, extend: bool) -> Did {
        self.leave_island();
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
        self.history.cut();
        self.inside = None;
        for sel in &mut self.sels {
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
        self.history.cut();
        self.inside = None;
        self.sels = vec![Sel::caret(self.text.clamp(at))];
    }

    pub fn extend_to(&mut self, at: Pos) {
        self.history.cut();
        self.inside = None;
        let at = self.text.clamp(at);
        if let Some(sel) = self.sels.last_mut() {
            sel.head = at;
        }
        self.merge_sels();
    }

    pub fn add_caret(&mut self, at: Pos) {
        self.history.cut();
        self.inside = None;
        self.sels.push(Sel::caret(self.text.clamp(at)));
        self.merge_sels();
    }

    /// キャレットが存在する場所を選択するために存在するものすべてを選択します (キャレットが含まれる構造の行、またはドキュメント全体)。
    pub fn select_all(&mut self) -> Did {
        if self.inside.is_some() {
            self.in_island(Inside::Extend, |editing| {
                editing.select_row();
                None
            });
            return Did::Moved;
        }
        self.history.cut();
        self.sels = vec![Sel::range(Pos::default(), self.text.end())];
        Did::Moved
    }

    pub fn set_sels(&mut self, sels: Vec<Sel>) {
        self.history.cut();
        self.inside = None;
        if sels.is_empty() {
            return;
        }
        self.sels = sels;
        self.merge_sels();
    }

    /// 余分なカーソルを削除し、フォーカスのあるカーソルを保持します。
    pub fn collapse_sels(&mut self) -> bool {
        if self.sels.len() == 1 {
            return false;
        }
        self.sels = vec![self.primary()];
        true
    }

    /// `Ctrl+D`: キャレットの単語を選択し、さらに押すたびに、同じテキストが表示される次の場所が追加されます。
    pub fn add_next_occurrence(&mut self) -> bool {
        // 構造体は 1 つのキャレットを保持するため、そこに追加するものは何もありません。
        if self.inside.is_some() {
            return false;
        }
        self.history.cut();
        let primary = self.primary();
        if primary.is_caret() {
            let Some(word) = word_at(&self.text, primary.head) else {
                return false;
            };
            *self.sels.last_mut().expect("a selection") = word;
            return true;
        }
        let needle: Vec<Item> = self
            .text
            .slice(primary.start(), primary.end())
            .into_iter()
            .next()
            .unwrap_or_default();
        if needle.is_empty() || primary.start().line != primary.end().line {
            return false;
        }
        let taken: Vec<Pos> = self.sels.iter().map(Sel::start).collect();
        let Some(found) = find_after(&self.text, &needle, primary.end(), &taken) else {
            return false;
        };
        self.sels.push(found);
        true
    }

    /// 選択内容がソートされ、重複がない状態が維持されるため、入力によって同じ編集が 2 回適用されることはありません。
    fn merge_sels(&mut self) {
        let primary = self.primary();
        self.sels.sort_by_key(|sel| (sel.start(), sel.end()));
        let mut merged: Vec<Sel> = Vec::with_capacity(self.sels.len());
        for sel in std::mem::take(&mut self.sels) {
            match merged.last_mut() {
                Some(last) if sel.start() <= last.end() => {
                    if sel.end() > last.end() {
                        *last = Sel::range(last.start(), sel.end());
                    }
                }
                _ => merged.push(sel),
            }
        }
        // 「プライマリ」がそれを意味し続けるように、フォーカスされた選択範囲は最後に残らなければなりません。
        if let Some(index) = merged
            .iter()
            .position(|sel| sel.start() <= primary.start() && primary.end() <= sel.end())
        {
            let focused = merged.remove(index);
            merged.push(focused);
        }
        self.sels = merged;
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
    let word = |col: usize| line.get(col).and_then(Item::as_char).is_some_and(is_word);
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
fn find_after(text: &Text, needle: &[Item], from: Pos, taken: &[Pos]) -> Option<Sel> {
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
        with_items(items_of(source))
    }

    pub(crate) fn with_items(lines: Vec<Vec<Item>>) -> Editor {
        let mut editor = Editor::default();
        editor.load(Text::from_lines(lines));
        editor
    }

    /// 文書のテキストは、各島が1つの文字として立っています。
    pub(crate) fn plain(editor: &Editor) -> String {
        (0..editor.text().line_count())
            .map(|line| {
                editor
                    .text()
                    .line(line)
                    .iter()
                    .map(|item| item.as_char().unwrap_or('\u{fffc}'))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_separator_is_one_item() {
        let mut editor = editor("x= 1");
        editor.set_caret(Pos::new(0, 1));
        editor.tab(false);
        assert_eq!(editor.text().item_at(Pos::new(0, 1)), Some(&Item::Tab));
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
    fn undo_takes_back_a_whole_multi_cursor_edit() {
        let mut editor = editor("ab\nab");
        editor.set_caret(Pos::new(0, 1));
        editor.add_caret(Pos::new(1, 1));
        editor.insert_text("X");
        editor.insert_text("Y");
        assert_eq!(plain(&editor), "aXYb\naXYb");
        // 入力は 1 つのステップに結合されるため、1 回元に戻すと両方の文字が消去されます。
        assert!(editor.undo());
        assert_eq!(plain(&editor), "ab\nab");
        assert!(editor.redo());
        assert_eq!(plain(&editor), "aXYb\naXYb");
    }

    #[test]
    fn backspace_joins_lines_and_deletes_an_island_whole() {
        let island = Item::Math(vec![crate::structure::ast::Node::Char('x')]);
        let mut editor = with_items(vec![vec![Item::Char('a'), island], vec![Item::Char('b')]]);
        editor.set_caret(Pos::new(1, 0));
        editor.backspace();
        assert_eq!(plain(&editor), "a\u{fffc}b");
        editor.set_caret(Pos::new(0, 2));
        editor.backspace();
        assert_eq!(plain(&editor), "ab");
    }

    #[test]
    fn enter_splits_and_undo_restores() {
        let mut editor = editor("ab");
        editor.set_caret(Pos::new(0, 1));
        editor.split_line();
        assert_eq!(plain(&editor), "a\nb");
        assert_eq!(editor.primary().head, Pos::new(1, 0));
        assert!(editor.undo());
        assert_eq!(plain(&editor), "ab");
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
    fn moving_down_keeps_the_column_within_the_line() {
        let mut editor = editor("long line\nab");
        editor.set_caret(Pos::new(0, 9));
        editor.move_v(true, false);
        assert_eq!(editor.primary().head, Pos::new(1, 2));
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
