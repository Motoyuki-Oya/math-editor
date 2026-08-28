//! ドキュメントの編集: 選択内容と、適用されるコマンド。
//!
//! すべてのコマンドは、複数のカーソルが予期される動作として、単一のステップとしてすべての選択に適用されます。ドキュメント自体は [`crate::structure::text`] であり、表記や画面については何も知りません。編集は行の入れ替えとして控えられ、文書の本体（元に戻す履歴を持つ側）へ引き渡されます。

mod cursor;
mod edit;
mod history;
mod movement;
mod nested;

pub use cursor::{merge_cursors, UnifiedCursor};
pub use history::Flush;
use history::{Recorder, Step};

use crate::structure::text::{Pos, Sel, Text};

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
    pub(super) fn moved(happened: bool) -> Did {
        if happened {
            Did::Moved
        } else {
            Did::Nothing
        }
    }
}

/// ドキュメントのモデル層: テキスト本体、Undo/Redo 履歴、変更行の管理。
/// 表記や画面、キャレット位置については何も知らず、複数ペイン間で共有されます。
#[derive(Default)]
pub struct Document {
    pub(crate) text: Text,
    pub(crate) recorder: Recorder,
    pub(crate) modified_lines: std::collections::BTreeSet<usize>,
}

impl Document {
    pub fn text(&self) -> &Text {
        &self.text
    }

    #[allow(dead_code)]
    pub fn text_mut(&mut self) -> &mut Text {
        &mut self.text
    }

    pub fn modified_lines(&self) -> Vec<usize> {
        self.modified_lines.iter().copied().collect()
    }

    pub fn clear_modified(&mut self) {
        self.modified_lines.clear();
    }

    pub fn mark_lines_modified(
        &mut self,
        from_line: usize,
        to_line: usize,
        end_line: usize,
    ) {
        let removed_lines = to_line.saturating_sub(from_line);
        let inserted_lines = end_line.saturating_sub(from_line);

        let mut next_modified = std::collections::BTreeSet::new();
        for &line in &self.modified_lines {
            if line < from_line {
                next_modified.insert(line);
            } else if line > to_line {
                let shifted = (line as isize + (inserted_lines as isize - removed_lines as isize))
                    .max(0) as usize;
                next_modified.insert(shifted);
            }
        }
        for l in from_line..=end_line {
            next_modified.insert(l);
        }
        self.modified_lines = next_modified;
    }

    pub fn load(&mut self, text: Text) {
        self.text = text;
        self.recorder = Recorder::default();
        self.clear_modified();
    }

    pub fn load_pending(&mut self, line_count: usize) {
        self.load(Text::pending(line_count));
    }

    pub fn resize_pending(&mut self, line_count: usize) {
        self.text.resize_pending(line_count);
    }

    pub fn resident_lines(&self) -> usize {
        self.text.line_count() - self.text.absent_lines()
    }

    pub fn evict_far(&mut self, keep: std::ops::Range<usize>, pinned: &[usize]) {
        self.text.evict_far(keep, pinned);
    }

    pub fn forget_range(&mut self, range: std::ops::Range<usize>) {
        self.text.forget_range(range);
    }

    pub fn feed(&mut self, from: usize, lines: Vec<crate::structure::text::SourceLine>) {
        for (offset, line) in lines.into_iter().enumerate() {
            self.text.fill_line(from + offset, line);
        }
    }


    #[allow(dead_code)]
    pub fn stats(&self) -> (usize, usize) {
        self.text.stats()
    }
}

pub struct Editor {
    pub document: Document,
    pub cursors: Vec<UnifiedCursor>,
}

impl std::ops::Deref for Editor {
    type Target = Document;

    fn deref(&self) -> &Self::Target {
        &self.document
    }
}

impl std::ops::DerefMut for Editor {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.document
    }
}

impl Default for Editor {
    fn default() -> Self {
        Self {
            document: Document::default(),
            cursors: vec![UnifiedCursor::caret(Pos::default())],
        }
    }
}

impl Editor {
    #[allow(dead_code)]
    pub fn cursors(&self) -> &[UnifiedCursor] {
        &self.cursors
    }

    /// 描画など本文の選択だけを見る境界で使う。
    #[allow(dead_code)]
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

    pub(super) fn primary_cursor(&self) -> &UnifiedCursor {
        self.cursors.last().expect("at least one selection")
    }

    pub(super) fn primary_cursor_mut(&mut self) -> &mut UnifiedCursor {
        self.cursors.last_mut().expect("at least one selection")
    }

    pub(super) fn has_inside(&self) -> bool {
        self.cursors.iter().any(|cursor| cursor.inside.is_some())
    }

    pub(super) fn clear_inside(&mut self) {
        for cursor in &mut self.cursors {
            cursor.inside = None;
            cursor.transient_structure = None;
        }
    }

    /// ファイルから読み取られたばかりのドキュメントを表示します。
    pub fn load(&mut self, text: Text) {
        self.document.load(text);
        self.cursors = vec![UnifiedCursor::caret(Pos::default())];
    }

    /// 読み込んだ内容をまるごと文書の本体へ届くようにする。本体が
    /// 1 行の空文書のときに使う（下書きの復元）。
    pub fn load_contents(&mut self, text: Text) {
        self.load(text);
        self.record(Step::Other);
        self.document.text.mark_all_changed();
    }

    /// 行数だけ分かっている文書を表示し、行は見えた場所から届く。
    #[allow(dead_code)]
    pub fn load_pending(&mut self, line_count: usize) {
        self.document.load_pending(line_count);
        self.cursors = vec![UnifiedCursor::caret(Pos::default())];
    }

    /// `keep` から遠い未編集の行を手放す。選択とキャレットの行は残す。
    #[allow(dead_code)]
    pub fn evict_far(&mut self, keep: std::ops::Range<usize>) {
        let pinned: Vec<usize> = self
            .cursors
            .iter()
            .flat_map(|sel| [sel.start().line, sel.end().line])
            .collect();
        self.document.evict_far(keep, &pinned);
    }

    /// 選択のいずれかが、まだ届いていない行に触れているか。届く前の行は
    /// 空に見えているだけなので、そこへの編集は中身を黙って壊してしまう。
    pub(super) fn touches_absent(&self) -> bool {
        if self.document.text.absent_lines() == 0 {
            return false;
        }
        self.cursors.iter().any(|cursor| {
            let (start, end) = (cursor.start().line, cursor.end().line);
            (start..=end).any(|line| self.document.text.is_absent(line))
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::structure::ast::{Cursor, Node, Row};
    use crate::structure::text::nodes_of;

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

    #[test]
    fn modified_lines_tracks_edits_and_line_shifts() {
        let mut editor = editor("line 1\nline 2\nline 3");
        assert_eq!(editor.modified_lines(), Vec::<usize>::new());

        // Edit line 1 (index 1)
        editor.set_caret(Pos::new(1, 0));
        editor.insert_text("edited ");
        assert_eq!(editor.modified_lines(), vec![1]);

        // Insert new line at line 0 (splits line 0 into lines 0 and 1, so line 1 becomes line 2)
        editor.set_caret(Pos::new(0, 0));
        editor.split_line();
        assert_eq!(editor.modified_lines(), vec![0, 1, 2]);

        // Clear modified
        editor.clear_modified();
        assert_eq!(editor.modified_lines(), Vec::<usize>::new());
    }

    /// 100万行の巨大スパース文書で、マルチカーソルを複数行に配置して
    /// 同時タイピング・改行・削除・変更記録を行うワークフローのシミュレーション。
    #[test]
    fn multi_cursor_typing_on_large_sparse_document_simulates_real_workflow() {
        use crate::structure::text::SourceLine;
        let mut editor = Editor::default();
        // 100万行のファイルを開いた状態
        editor.load_pending(1_000_000);
        assert_eq!(editor.text().line_count(), 1_000_000);
        assert_eq!(editor.text().absent_lines(), 1_000_000);

        // 画面に表示される 50,000〜50,030 行を取り寄せる
        let resident_lines: Vec<SourceLine> = (50_000..=50_030)
            .map(|i| SourceLine::Plain(format!("fn item_{i}() {{ return {i}; }}")))
            .collect();
        editor.feed(50_000, resident_lines);
        assert_eq!(editor.text().absent_lines(), 1_000_000 - 31);

        // 50,000行目、50,010行目、50,020行目の3箇所にマルチカーソルを配置（末尾）
        let p1 = Pos::new(50_000, editor.text().line_len(50_000));
        let p2 = Pos::new(50_010, editor.text().line_len(50_010));
        let p3 = Pos::new(50_020, editor.text().line_len(50_020));
        editor.set_sels(vec![Sel::caret(p1), Sel::caret(p2), Sel::caret(p3)]);
        assert_eq!(editor.cursors().len(), 3);

        // 3箇所で同時に " // done" をタイピング
        let start_time = std::time::Instant::now();
        for c in " // done".chars() {
            editor.insert_text(&c.to_string());
        }
        // 1文字ずつのマルチカーソルタイピングが瞬時に完了すること
        assert!(start_time.elapsed() < std::time::Duration::from_millis(50));

        // 各行の内容を検証
        assert_eq!(
            plain_line(&editor, 50_000),
            "fn item_50000() { return 50000; } // done"
        );
        assert_eq!(
            plain_line(&editor, 50_010),
            "fn item_50010() { return 50010; } // done"
        );
        assert_eq!(
            plain_line(&editor, 50_020),
            "fn item_50020() { return 50020; } // done"
        );

        // 各行で同時に backspace を 3 回実行 ("ne" と 空白が消えて "// do" になる)
        editor.backspace();
        editor.backspace();
        editor.backspace();
        assert_eq!(
            plain_line(&editor, 50_000),
            "fn item_50000() { return 50000; } // d"
        );

        // 変更行記録（FlushBatch）の取得
        let flush = editor.take_flush();
        assert!(flush.is_some());
        let flush = flush.unwrap();
        assert!(!flush.changes.is_empty());
        // 50000, 50010, 50020 の各行が変更行として記録されていること
        assert!(flush.changes.iter().any(|c| c.from == 50_000));
        assert!(flush.changes.iter().any(|c| c.from == 50_010));
        assert!(flush.changes.iter().any(|c| c.from == 50_020));

        // 未着の行（90万行目など）は展開されずにAbsentのままであること
        assert!(editor.text().is_absent(900_000));
    }

    /// 巨大ファイル上で Ctrl+D（add_next_occurrence）による単語マッチングと
    /// マルチカーソル一括置換のシミュレーション。
    #[test]
    fn multi_cursor_ctrl_d_and_replace_on_large_document() {
        use crate::structure::text::SourceLine;
        let mut editor = Editor::default();
        editor.load_pending(500_000);

        // 100,000〜100,010 行に同一キーワード "target_keyword" を含む行を配置
        let mut lines = Vec::new();
        for i in 0..10 {
            lines.push(SourceLine::Plain(format!(
                "let value_{i} = target_keyword + {i};"
            )));
        }
        editor.feed(100_000, lines);

        // 100,000 行目の "target_keyword" の位置にキャレットを置いて Ctrl+D を押す
        let col = "let value_0 = ".len();
        editor.set_caret(Pos::new(100_000, col));
        assert!(editor.add_next_occurrence()); // 1つ目の単語を選択
        assert_eq!(
            editor.primary(),
            Sel::range(
                Pos::new(100_000, col),
                Pos::new(100_000, col + "target_keyword".len())
            )
        );

        // さらに Ctrl+D を 3 回押して後続 3 行の "target_keyword" も選択
        assert!(editor.add_next_occurrence());
        assert!(editor.add_next_occurrence());
        assert!(editor.add_next_occurrence());
        assert_eq!(editor.cursors().len(), 4);

        // 4箇所を一括で "NEW_VAL" に置換（タイピング）
        editor.insert_text("NEW_VAL");

        // 各行が正しく置換されたことを検証
        assert_eq!(plain_line(&editor, 100_000), "let value_0 = NEW_VAL + 0;");
        assert_eq!(plain_line(&editor, 100_001), "let value_1 = NEW_VAL + 1;");
        assert_eq!(plain_line(&editor, 100_002), "let value_2 = NEW_VAL + 2;");
        assert_eq!(plain_line(&editor, 100_003), "let value_3 = NEW_VAL + 3;");
    }

    fn plain_line(editor: &Editor, line: usize) -> String {
        crate::structure::plain::row(&editor.text().line(line).to_vec())
    }
}
