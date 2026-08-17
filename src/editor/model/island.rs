//! 島への橋: 出入りし、内部で編集を行う [`crate::structural::edit`] にコマンドを渡します。
//!
//! キャレットはどちらの場合でも 1 つのドキュメント内で 1 か所です。 「内部」は、それがその場所の深さまで到達することを示すだけです。構造内で別の意味を持つコマンド (Tab、Enter、矢印) は、[`super::Editor`] 独自のメソッドに留まり、ここで呼び出します。

use super::history::Step;
use super::Editor;
use crate::structure::ast::{row_at, Cursor, Node, Row};
use crate::structure::edit::{Editing, Escape};
use crate::structure::text::{before_col, before_pos, Item, Pos, Sel};

/// アイランド内のコマンドがドキュメントに対して行うことは、元に戻す履歴への結合方法とファイルがダーティになったかどうかの両方を決定します。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Inside {
    /// キャレットのみが移動しました。
    Move,
    /// 選択範囲は拡大または縮小されました。アンカーはその場に留まります。
    Extend,
    /// 文字が入力され、その前のステップに結合されました。
    Type,
    /// 構造が別の方法で変更されました。
    Change,
}

impl Editor {
    /// キャレットが島内にある場合は、その島の中にあります。
    pub fn inside(&self) -> Option<&Cursor> {
        self.inside.as_ref()
    }

    /// キャレットに空の島を置き、そこにステップインします。
    pub fn insert_island(&mut self) {
        self.insert_math(Row::new());
        let at = before_pos(self.primary().head);
        self.sels = vec![Sel::caret(at)];
        self.inside = Some(Cursor::root(0));
        // 式の開始はそれ自体が独立したステップであるため、式に入力した内容を元に戻しても式は削除されません。
        self.history.cut();
    }

    /// 左または右から、`at` のアイランドにステップインします。
    pub fn enter_island(&mut self, at: Pos, from_start: bool) -> bool {
        if !matches!(self.text.item_at(at), Some(Item::Math(_))) {
            return false;
        }
        self.sels = vec![Sel::caret(at)];
        self.inside = Some(Cursor::default());
        self.in_island(Inside::Move, |editing| {
            if from_start {
                editing.move_to_start();
            } else {
                editing.move_to_end();
            }
            None
        })
    }

    /// クリックが着いたキャレットとともに、`at` のアイランドにステップインします。
    pub fn enter_island_at(&mut self, at: Pos, cursor: &Cursor) -> bool {
        if !self.enter_island(at, true) {
            return false;
        }
        let to = cursor.clone();
        self.in_island(Inside::Move, move |editing| {
            editing.set_cursor(to);
            None
        })
    }

    /// キャレットをその横に残して、アイランドから戻ります。
    pub fn leave_island(&mut self) -> bool {
        if self.inside.take().is_none() {
            return false;
        }
        let at = self.primary().head;
        self.history.cut();
        self.sels = vec![Sel::caret(self.text.clamp(Pos::new(at.line, at.col + 1)))];
        true
    }

    /// キャレットが存在するアイランドでコマンドを実行します。アイランドは存在する場所で編集されるため、コマンドはドキュメント自体のステップになります。履歴。
    pub fn in_island(
        &mut self,
        kind: Inside,
        command: impl FnOnce(&mut Editing<'_>) -> Option<Escape>,
    ) -> bool {
        // カーソルは取得されず、コピーされます。履歴はコマンドが実行される前に書き込まれ、キャレットが内部にあった場所を記憶する必要があります。
        let Some(mut cursor) = self.inside.clone() else {
            return false;
        };
        let at = self.primary().head;
        match kind {
            Inside::Move | Inside::Extend => self.history.cut(),
            Inside::Type => self.record(Step::Typing),
            Inside::Change => self.record(Step::Other),
        }
        let Some(root) = self.text.math_at_mut(at) else {
            return false;
        };
        let escape = command(&mut Editing::new(root, &mut cursor));
        self.inside = Some(cursor);
        match escape {
            // 数式を超えた選択範囲は、他のものと同様にテキストの 1 つの項目である数式の選択範囲になります。
            Some(_) if kind == Inside::Extend => {
                self.inside = None;
                let after = self.text.clamp(Pos::new(at.line, at.col + 1));
                self.sels = vec![Sel::range(at, after)];
                true
            }
            Some(escape) => {
                self.escape_island(at, escape, !matches!(kind, Inside::Move | Inside::Extend))
            }
            None => true,
        }
    }

    /// アイランド内の行の範囲を `at` で表示します。これにより、構造内で見つかった一致が選択される方法が示されます。
    pub fn select_in_island(&mut self, at: Pos, cursor: Cursor) -> bool {
        if !matches!(self.text.item_at(at), Some(Item::Math(_))) {
            return false;
        }
        self.history.cut();
        self.sels = vec![Sel::caret(at)];
        self.inside = Some(cursor);
        true
    }

    /// アイランド内の行の範囲を置き換えます。 「で」で。置換は単純な文字として入力されるため、置換によって誤って構造が構築されることはありません。
    pub fn replace_in_island(&mut self, at: Pos, cursor: Cursor, with: &str) -> bool {
        if !self.select_in_island(at, cursor) {
            return false;
        }
        // 構造には 1 つの行が保持され、列の区切り文字が含まれないため、どちらも意味を持ちません。
        let nodes: Row = with
            .chars()
            .filter(|c| *c != '\n' && *c != '\t')
            .map(Node::Char)
            .collect();
        self.insert_row_in_island(nodes)
    }

    /// キャレットが内側にある島をテキストの 1 つの項目として選択します。これが構造を超えた選択を意味します。
    pub fn select_island(&mut self) -> bool {
        if self.inside.take().is_none() {
            return false;
        }
        let at = self.primary().head;
        self.history.cut();
        let after = self.text.clamp(Pos::new(at.line, at.col + 1));
        self.sels = vec![Sel::range(at, after)];
        true
    }

    /// アンカーを維持したまま、数式内の選択範囲を `カーソル` にドラッグします。
    pub fn extend_in_island(&mut self, cursor: &Cursor) -> bool {
        let to = cursor.clone();
        self.in_island(Inside::Extend, move |editing| {
            editing.extend_to(&to);
            None
        })
    }

    /// 数式内の選択を構造化します。
    pub fn island_selection(&self) -> Option<Row> {
        let cursor = self.inside.as_ref()?;
        if cursor.is_caret() {
            return None;
        }
        let Some(Item::Math(root)) = self.text.item_at(self.primary().head) else {
            return None;
        };
        let row = row_at(root, &cursor.path)?;
        Some(row[cursor.start()..cursor.end().min(row.len())].to_vec())
    }

    pub fn insert_in_island(&mut self, node: Node) -> bool {
        self.in_island(Inside::Change, |editing| {
            editing.insert(node);
            None
        })
    }

    pub fn insert_row_in_island(&mut self, nodes: Row) -> bool {
        self.in_island(Inside::Change, |editing| {
            editing.insert_row(nodes);
            None
        })
    }

    pub(super) fn type_in_island(&mut self, c: char) -> bool {
        let mut left = false;
        let done = self.in_island(Inside::Type, |editing| {
            // コマンドとして入力された名前の最後にはスペースが入ります。ショートカットは、キーボード ハンドラーではなく構造体に属します。
            if c == ' ' && editing.commit_command() {
                return None;
            }
            let escape = editing.insert_char(c);
            left = escape.is_some();
            escape
        });
        // `1/` を入力して開始された数式は、分数が書き込まれると終了します。そのため、その後に続くものは再びテキストになり、数式内ではなくそこに書き込まれます。
        if done && left {
            let mut buffer = [0u8; 4];
            self.insert_text(c.encode_utf8(&mut buffer));
        }
        done
    }

    /// 入力されている構造体が書き込まれるまでのみ持続するものとして数式をマークします。これが、`1/` などのトリガーによって開始される数式の目的です。テキストに戻った後に入力されたものはすべてテキストに戻ります。
    pub fn island_lasts_one_structure(&mut self) {
        if let Some(cursor) = self.inside.as_mut() {
            cursor.fills.insert(0, 0);
        }
    }

    /// キャレットが出た島を残し、空の島を持ち出します。何も残っていない数式の先頭からバックスペースで移動すると、数式が削除されます。
    pub(super) fn escape_island(&mut self, at: Pos, escape: Escape, recorded: bool) -> bool {
        let empty = matches!(self.text.item_at(at), Some(Item::Math(row)) if row.is_empty());
        self.inside = None;
        let after = self.text.clamp(Pos::new(at.line, at.col + 1));
        match escape {
            Escape::Delete | Escape::Left if empty => {
                if !recorded {
                    self.record(Step::Other);
                }
                self.text.remove(at, after);
                self.sels = vec![Sel::caret(at)];
            }
            Escape::Left => self.sels = vec![Sel::caret(at)],
            _ => self.sels = vec![Sel::caret(after)],
        }
        true
    }

    /// キャレットが移動しようとしている数式がある場合は、その数式にステップインします。
    pub(super) fn enter_island_beside(&mut self, forward: bool) -> bool {
        let sel = self.primary();
        if !sel.is_caret() || self.sels.len() != 1 {
            return false;
        }
        let at = if forward {
            Some(sel.head)
        } else {
            before_col(sel.head)
        };
        at.is_some_and(|at| self.enter_island(at, forward))
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{editor, plain, with_items};
    use super::*;
    use crate::editor::clipboard::Clip;

    /// 構造としてキャレットが立っている島。
    fn island(editor: &Editor) -> Row {
        match editor.text().item_at(editor.primary().head) {
            Some(Item::Math(row)) => row.clone(),
            other => panic!("expected an island, got {other:?}"),
        }
    }

    fn started_in_an_island() -> Editor {
        let mut editor = editor("a");
        editor.set_caret(Pos::new(0, 1));
        editor.insert_island();
        editor
    }

    #[test]
    fn a_formula_is_typed_into_the_document_itself() {
        let mut editor = started_in_an_island();
        editor.type_in_island('x');
        assert_eq!(island(&editor), vec![Node::Char('x')]);
        assert_eq!(plain(&editor), "a\u{fffc}");
    }

    /// 数式内の入力は、ドキュメントの履歴なので、1 回元に戻すとキャレットが数式に戻ります。
    #[test]
    fn undo_takes_back_typing_inside_a_formula() {
        let mut editor = started_in_an_island();
        editor.type_in_island('x');
        editor.type_in_island('y');
        assert!(editor.undo());
        assert_eq!(island(&editor), Vec::new());
        assert!(editor.inside().is_some());
        assert!(editor.redo());
        assert_eq!(island(&editor).len(), 2);
    }

    /// キャレットは、作成したばかりの分数の下の行に移動します。これは、次の文字が属する空のスロットです。
    #[test]
    fn the_caret_lands_in_the_empty_slot_of_a_new_fraction() {
        let mut editor = started_in_an_island();
        editor.type_in_island('1');
        editor.type_in_island('/');
        let cursor = editor.inside().expect("inside the formula");
        assert_eq!(cursor.path, vec![(0, 1)]);
        assert_eq!(cursor.index, 0);
        editor.type_in_island('2');
        assert_eq!(editor.inside().expect("inside the formula").index, 1);
    }

    #[test]
    fn selecting_inside_a_formula_takes_the_structure_it_reaches() {
        let mut editor = started_in_an_island();
        for c in "1/2".chars() {
            editor.type_in_island(c);
        }
        // 下の行内: `2` を選択し、次に分数そのものを選択します。
        editor.in_island(Inside::Extend, |editing| editing.extend(false));
        assert_eq!(editor.island_selection(), Some(vec![Node::Char('2')]));
        editor.in_island(Inside::Extend, |editing| editing.extend(false));
        let selected = editor.island_selection().expect("a selection");
        assert!(matches!(selected.as_slice(), [Node::Stack { .. }]));
    }

    /// 数式内のすべてを選択すると、数式の選択になります。文字などのテキストの 1 項目です。
    #[test]
    fn a_selection_that_outgrows_a_formula_selects_the_formula() {
        let mut editor = started_in_an_island();
        editor.type_in_island('x');
        editor.in_island(Inside::Extend, |editing| editing.extend(false));
        editor.in_island(Inside::Extend, |editing| editing.extend(false));
        assert!(editor.inside().is_none());
        assert_eq!(editor.primary(), Sel::range(Pos::new(0, 1), Pos::new(0, 2)));
    }

    #[test]
    fn typing_over_a_selection_inside_a_formula_replaces_it() {
        let mut editor = started_in_an_island();
        for c in "xy".chars() {
            editor.type_in_island(c);
        }
        editor.in_island(Inside::Extend, |editing| editing.extend(false));
        editor.type_in_island('z');
        assert_eq!(island(&editor), vec![Node::Char('x'), Node::Char('z')]);
    }

    #[test]
    fn moving_off_a_selection_inside_a_formula_leaves_a_caret() {
        let mut editor = started_in_an_island();
        for c in "xy".chars() {
            editor.type_in_island(c);
        }
        editor.in_island(Inside::Extend, |editing| editing.extend(false));
        editor.in_island(Inside::Move, |editing| editing.move_left());
        let cursor = editor.inside().expect("inside the formula");
        assert!(cursor.is_caret());
        assert_eq!(cursor.index, 1);
    }

    /// 終了何も残っていない数式の先頭に、1 回の元に戻す手順で数式も取り込まれます。
    #[test]
    fn backspacing_out_of_an_empty_formula_removes_it() {
        let mut editor = started_in_an_island();
        editor.in_island(Inside::Change, |editing| editing.backspace());
        assert!(editor.inside().is_none());
        assert_eq!(plain(&editor), "a");
        assert!(editor.undo());
        assert_eq!(plain(&editor), "a\u{fffc}");
    }

    #[test]
    fn entering_a_formula_from_the_right_puts_the_caret_at_its_end() {
        let mut editor = started_in_an_island();
        for c in "xy".chars() {
            editor.type_in_island(c);
        }
        assert!(editor.leave_island());
        assert_eq!(editor.primary().head, Pos::new(0, 2));
        assert!(editor.enter_island(Pos::new(0, 1), false));
        assert_eq!(editor.inside().expect("inside the formula").index, 2);
        assert!(editor.enter_island(Pos::new(0, 1), true));
        assert_eq!(editor.inside().expect("inside the formula").index, 0);
    }

    /// テキスト内でキャレットを移動すると、数式は残ります。キャレットは 1 つあり、各場所に 1 つずつあるわけではありません。
    #[test]
    fn moving_in_the_text_leaves_the_formula() {
        let mut editor = started_in_an_island();
        editor.move_h(false, false);
        assert!(editor.inside().is_none());
    }

    /// 数式を横切って右に移動すると、上ではなく数式内にステップインするため、同じキーで最後まで同じ動作が行われます。
    #[test]
    fn moving_across_a_formula_steps_inside_it() {
        let mut editor = started_in_an_island();
        editor.type_in_island('x');
        assert!(editor.leave_island());
        editor.set_caret(Pos::new(0, 1));
        editor.move_h(true, false);
        assert_eq!(editor.inside().expect("inside the formula").index, 0);
        editor.move_h(true, false);
        assert_eq!(editor.inside().expect("inside the formula").index, 1);
        // もう 1 ステップでキャレットが反対側に移動します。
        editor.move_h(true, false);
        assert!(editor.inside().is_none());
        assert_eq!(editor.primary().head, Pos::new(0, 2));
    }

    /// どこに入力しても 1 つのコマンドが実行されます。キャレットはです。
    #[test]
    fn typing_reaches_whichever_place_the_caret_is_in() {
        let mut editor = started_in_an_island();
        editor.insert_text("1");
        editor.insert_text("/");
        editor.insert_text("2");
        assert!(matches!(island(&editor).as_slice(), [Node::Stack { .. }]));
        editor.leave_island();
        editor.insert_text("b");
        assert_eq!(plain(&editor), "a\u{fffc}b");
    }

    /// 数式内の貼り付けは、入力ではなくテキストです。「a/b」は分数になる代わりに 3 文字のままです。
    #[test]
    fn a_paste_inside_a_formula_stays_plain() {
        let mut editor = started_in_an_island();
        editor.insert_text("a/b");
        assert_eq!(
            island(&editor),
            vec![Node::Char('a'), Node::Char('/'), Node::Char('b')]
        );
    }

    /// タブはテキスト内の列の区切り文字であり、数式内の次のスロットです。
    #[test]
    fn tab_steps_to_the_next_slot_inside_a_formula() {
        let mut editor = started_in_an_island();
        editor.insert_text("1");
        editor.insert_text("/");
        // 分数の下の行。 Tab キーを押すと、区切り文字を入れるのではなく、分数を保持する行がそのまま残ります。
        editor.tab(false);
        assert!(island(&editor)
            .iter()
            .all(|node| !matches!(node, Node::Char('\t'))));
        assert_eq!(
            editor.inside().expect("inside the formula").path,
            Vec::new()
        );
    }

    /// 分数の下の行は 1 回実行され、キャレットが戻されます。そのため、次に入力された内容は分数の下ではなく横に配置されます。
    #[test]
    fn typing_on_past_a_fraction_leaves_it_behind() {
        let mut editor = started_in_an_island();
        for c in "1/2 + 3".chars() {
            editor.type_in_island(c);
        }
        assert_eq!(
            editor.inside().expect("inside the formula").path,
            Vec::new()
        );
        let row = island(&editor);
        assert!(matches!(row.first(), Some(Node::Stack { .. })));
        let after: String = row[1..]
            .iter()
            .filter_map(|node| match node {
                Node::Char(c) => Some(*c),
                _ => None,
            })
            .collect();
        assert_eq!(after, " + 3");
    }

    /// トリガーがそこに置いた式は、それを呼び出した構造を保持するだけで、それ以上は何も保持しません。テキストになった後に再度入力されるものです。
    #[test]
    fn a_formula_a_trigger_made_ends_with_its_structure() {
        let mut editor = started_in_an_island();
        editor.island_lasts_one_structure();
        for c in "1/2 + 3".chars() {
            editor.insert_text(&c.to_string());
        }
        assert!(editor.inside().is_none());
        editor.set_caret(Pos::new(0, 1));
        assert_eq!(island(&editor).len(), 1);
        assert!(matches!(island(&editor).first(), Some(Node::Stack { .. })));
        assert_eq!(plain(&editor), "a\u{fffc} + 3");
    }

    /// Enter キーと Escape キーを押すと、行を分割する代わりに数式が終了します。
    #[test]
    fn enter_and_escape_leave_a_formula() {
        let mut editor = started_in_an_island();
        editor.insert_text("x");
        editor.split_line();
        assert!(editor.inside().is_none());
        assert_eq!(plain(&editor), "a\u{fffc}");
        editor.enter_island(Pos::new(0, 1), true);
        editor.escape();
        assert!(editor.inside().is_none());
    }

    /// 一致構造体内で見つかったものは、見つかった場所に置き換えられ、置換された文字はプレーンな文字のままです。
    #[test]
    fn a_replacement_reaches_inside_a_formula() {
        let mut editor = started_in_an_island();
        for c in "ab".chars() {
            editor.type_in_island(c);
        }
        editor.leave_island();
        let at = Pos::new(0, 1);
        let found = Cursor {
            path: Vec::new(),
            anchor: 0,
            index: 1,
            fills: Vec::new(),
        };
        assert!(editor.replace_in_island(at, found, "x/y"));
        editor.set_caret(at);
        assert_eq!(
            island(&editor),
            vec![
                Node::Char('x'),
                Node::Char('/'),
                Node::Char('y'),
                Node::Char('b'),
            ]
        );
        assert!(editor.undo());
        assert_eq!(island(&editor), vec![Node::Char('a'), Node::Char('b')]);
    }

    /// テキストからコピーされた構造は、読み取られる文字としてではなく、構造として戻されます。
    #[test]
    fn a_copied_structure_pastes_back_as_itself() {
        let fraction = vec![Node::Stack {
            above: vec![Node::Char('a')],
            below: vec![Node::Char('b')],
            between: crate::structure::ast::Between::Rule,
        }];
        let mut editor = with_items(vec![vec![Item::Math(fraction.clone())]]);
        editor.set_caret(Pos::new(0, 1));
        let clip = Clip::Text(vec![vec![Item::Math(fraction.clone())]]);
        editor.insert_clip(&clip);
        assert_eq!(
            editor.text().line(0),
            &[Item::Math(fraction.clone()), Item::Math(fraction.clone()),]
        );
    }

    /// 構造は 1 行なので、ファイルに保持できなかった文字として挿入されるのではなく、貼り付けられた改行が削除されます。
    #[test]
    fn pasting_lines_inside_a_structure_keeps_one_row() {
        let mut editor = started_in_an_island();
        editor.insert_text("ab\ncd");
        assert_eq!(
            island(&editor),
            vec![
                Node::Char('a'),
                Node::Char('b'),
                Node::Char('c'),
                Node::Char('d')
            ]
        );
    }

    /// 構造に貼り付けると、コピーされた部分がその行に配置されるため、分母に貼り付けられた分数はそこにある分数になります。
    #[test]
    fn a_copied_piece_pastes_inside_a_structure() {
        let mut editor = started_in_an_island();
        let piece: Row = vec![Node::Char('a'), Node::Char('b')];
        editor.insert_clip(&Clip::Row(piece));
        assert_eq!(island(&editor), vec![Node::Char('a'), Node::Char('b')]);
    }

    /// 構造内にはキャレットが 1 つあるため、Ctrl+D はキャレットが置かれているテキストの単語を選択する以外に何もしません。
    #[test]
    fn ctrl_d_does_nothing_inside_a_formula() {
        let mut editor = started_in_an_island();
        editor.insert_text("a");
        editor.insert_text("b");
        assert!(!editor.add_next_occurrence());
        assert_eq!(editor.sels().len(), 1);
        assert!(editor.inside().is_some());
        // 入力は構造内に進みます。
        editor.insert_text("c");
        assert_eq!(
            island(&editor),
            vec![Node::Char('a'), Node::Char('b'), Node::Char('c')]
        );
    }

    /// 入力されたテキストを構造に変換することは歴史の 1 ステップです。元に戻すと、入力された数式ではなく文字が戻ります。
    #[test]
    fn a_shortcut_that_builds_a_structure_is_one_undo() {
        let mut editor = editor("x1");
        editor.set_caret(Pos::new(0, 2));
        editor.one_step(|editor| {
            editor.replace_range(Pos::new(0, 0), Pos::new(0, 2), "");
            editor.insert_island();
            for c in "x1/".chars() {
                editor.insert_text(&c.to_string());
            }
        });
        assert!(matches!(island(&editor).as_slice(), [Node::Stack { .. }]));
        assert!(editor.undo());
        assert_eq!(plain(&editor), "x1");
        // その間には何もありません。構築途中の数式は決してステップではありません。
        assert!(!editor.undo());
        assert!(editor.redo());
        assert!(matches!(island(&editor).as_slice(), [Node::Stack { .. }]));
    }

    /// 内部の選択範囲数式はドラッグできます。これはキーボードによる選択と同じです。
    #[test]
    fn dragging_inside_a_formula_selects_the_same_way() {
        let mut editor = started_in_an_island();
        for c in "abc".chars() {
            editor.type_in_island(c);
        }
        editor.in_island(Inside::Move, |editing| {
            editing.move_to_start();
            None
        });
        assert!(editor.extend_in_island(&Cursor {
            path: Vec::new(),
            anchor: 0,
            index: 2,
            fills: Vec::new(),
        }));
        assert_eq!(
            editor.island_selection(),
            Some(vec![Node::Char('a'), Node::Char('b')])
        );
        // 数式の外にドラッグすると、全体が取り出されます。
        assert!(editor.select_island());
        assert_eq!(editor.primary(), Sel::range(Pos::new(0, 1), Pos::new(0, 2)));
    }
}
