use super::cursor::{shifted, UnifiedCursor};
use super::history::Step;
use super::movement::{after, before};
use super::nested::Inside;
use super::{Did, Editor};
use crate::editor::clipboard::Clip;
use crate::structure::ast::{Cursor, Node, Row};
use crate::structure::text::{nodes_of, Pos, Sel, Text};

pub fn matching_bracket(c: char) -> Option<char> {
    match c {
        '(' => Some(')'),
        '{' => Some('}'),
        '[' => Some(']'),
        '"' => Some('"'),
        '\'' => Some('\''),
        '「' => Some('」'),
        '（' => Some('）'),
        '『' => Some('』'),
        '【' => Some('】'),
        '《' => Some('》'),
        '〈' => Some('〉'),
        '〔' => Some('〕'),
        _ => None,
    }
}

impl Editor {
    pub(super) fn edit_each(
        &mut self,
        step: Step,
        edit: impl Fn(&Text, Sel) -> (Pos, Pos, Vec<Row>),
    ) {
        let order: Vec<usize> = self
            .cursors
            .iter()
            .enumerate()
            .filter_map(|(index, cursor)| cursor.inside.is_none().then_some(index))
            .collect();
        self.edit_indices(step, order, edit);
    }

    pub(super) fn edit_indices(
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
            self.mark_lines_modified(from.line, to.line, end.line);
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

    pub(super) fn insert_indices(&mut self, what: Vec<Row>, indices: Vec<usize>) {
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
        self.insert_text_with_mode(text, false)
    }

    pub fn insert_text_with_mode(&mut self, text: &str, overwrite: bool) -> Did {
        if self.touches_absent() {
            return Did::Nothing;
        }
        if overwrite && !self.has_inside() && !text.contains('\n') {
            for cursor in self.cursors.iter_mut().filter(|c| c.inside.is_none()) {
                if cursor.sel.is_caret() {
                    let head = cursor.sel.head;
                    let line_len = self.document.text.line_len(head.line);
                    if head.col < line_len {
                        let next_col = crate::structure::text::character_after(
                            self.document.text.line(head.line),
                            head.col,
                        );
                        cursor.sel = Sel::range(head, Pos::new(head.line, next_col));
                    }
                }
            }
        }
        let mut chars = text.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            if self.convert_typed(c) {
                return Did::Changed;
            }
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

        if !top_level.is_empty() {
            let mut chars = text.chars();
            match (chars.next(), chars.next(), chars.next()) {
                // 2文字のペア（日本語変換等で「【】」や「『』」などが一度に入力された場合）
                (Some(open), Some(close), None) if matching_bracket(open) == Some(close) => {
                    let any_selected = top_level.iter().any(|&i| !self.cursors[i].sel.is_caret());
                    if any_selected {
                        self.wrap_selection_with(open, close, top_level);
                        return Did::Changed;
                    }
                    self.insert_bracket_pair(open, close, top_level);
                    return Did::Changed;
                }
                // 1文字の入力
                (Some(c), None, _) => {
                    if let Some(closing) = matching_bracket(c) {
                        let any_selected =
                            top_level.iter().any(|&i| !self.cursors[i].sel.is_caret());
                        if any_selected {
                            self.wrap_selection_with(c, closing, top_level);
                            return Did::Changed;
                        } else if (c == '"' || c == '\'')
                            && top_level.iter().all(|&i| {
                                let head = self.cursors[i].sel.head;
                                let line = self.document.text.line(head.line);
                                line.get(head.col).and_then(crate::structure::text::as_char)
                                    == Some(c)
                            })
                        {
                            for &i in &top_level {
                                let head = self.cursors[i].sel.head;
                                let next = Pos::new(head.line, head.col + 1);
                                self.cursors[i] = UnifiedCursor::caret(next);
                            }
                            return Did::Moved;
                        } else {
                            self.insert_bracket_pair(c, closing, top_level);
                            return Did::Changed;
                        }
                    } else if matches!(
                        c,
                        ')' | '}' | ']' | '」' | '）' | '』' | '】' | '》' | '〉' | '〕'
                    ) {
                        let can_overtype = top_level.iter().all(|&i| {
                            if !self.cursors[i].sel.is_caret() {
                                return false;
                            }
                            let head = self.cursors[i].sel.head;
                            let line = self.document.text.line(head.line);
                            line.get(head.col).and_then(crate::structure::text::as_char) == Some(c)
                        });
                        if can_overtype {
                            for &i in &top_level {
                                let head = self.cursors[i].sel.head;
                                let next = Pos::new(head.line, head.col + 1);
                                self.cursors[i] = UnifiedCursor::caret(next);
                            }
                            return Did::Moved;
                        }
                    }
                }
                _ => {}
            }
        }

        self.insert_indices(nodes_of(text), top_level);
        Did::Changed
    }

    fn wrap_selection_with(&mut self, open: char, close: char, mut order: Vec<usize>) {
        if order.is_empty() {
            return;
        }
        self.record(Step::Other);
        order.sort_by_key(|&i| self.cursors[i].start());
        for (done, &i) in order.iter().enumerate() {
            let sel = self.cursors[i].sel;
            let start = sel.start();
            let end = sel.end();
            let inner = self.document.text.slice(start, end);
            let mut wrapped_rows = inner;
            if let Some(first) = wrapped_rows.first_mut() {
                first.insert(0, Node::char(open));
            } else {
                wrapped_rows = vec![vec![Node::char(open)]];
            }
            if let Some(last) = wrapped_rows.last_mut() {
                last.push(Node::char(close));
            }
            let at = self.document.text.remove(start, end);
            let new_end = self.document.text.insert(at, wrapped_rows);
            self.mark_lines_modified(start.line, end.line, new_end.line);
            let new_sel_start = Pos::new(start.line, start.col + 1);
            let new_sel_end = if start.line == end.line {
                Pos::new(end.line, end.col + 1)
            } else {
                Pos::new(end.line, end.col)
            };
            self.cursors[i] = UnifiedCursor::range(new_sel_start, new_sel_end);
            for &later in &order[done + 1..] {
                let s = self.cursors[later].sel;
                self.cursors[later] = UnifiedCursor::range(
                    shifted(s.anchor, end, new_end),
                    shifted(s.head, end, new_end),
                );
            }
        }
        self.merge_sels();
    }

    fn insert_bracket_pair(&mut self, open: char, close: char, mut order: Vec<usize>) {
        if order.is_empty() {
            return;
        }
        self.record(Step::Typing);
        order.sort_by_key(|&i| self.cursors[i].start());
        for (done, &i) in order.iter().enumerate() {
            let sel = self.cursors[i].sel;
            let from = sel.start();
            let to = sel.end();
            let pair_nodes = vec![vec![Node::char(open), Node::char(close)]];
            let at = self.document.text.remove(from, to);
            let end = self.document.text.insert(at, pair_nodes);
            self.mark_lines_modified(from.line, to.line, end.line);
            let caret_pos = Pos::new(from.line, from.col + 1);
            self.cursors[i] = UnifiedCursor::caret(caret_pos);
            for &later in &order[done + 1..] {
                let s = self.cursors[later].sel;
                self.cursors[later] =
                    UnifiedCursor::range(shifted(s.anchor, to, end), shifted(s.head, to, end));
            }
        }
        self.merge_sels();
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
                }),
            }];
            return Did::Changed;
        }
        Did::Nothing
    }

    /// 直前の漢字文字列にルビ（上付きアノテーション）を自動適用します。
    pub fn apply_ruby(&mut self, kanji_len: usize, reading: &str) -> Did {
        if self.touches_absent() || self.has_inside() {
            return Did::Nothing;
        }
        let sel = self.primary();
        if !sel.is_caret() || sel.head.col < kanji_len {
            return Did::Nothing;
        }
        let line = sel.head.line;
        let start = Pos::new(line, sel.head.col - kanji_len);
        let end = sel.head;
        let lines = self.text.slice(start, end);
        let Some(items) = lines.first() else {
            return Did::Nothing;
        };
        let mut node = Node::container(items.clone());
        node.upper = reading.chars().map(Node::char).collect();
        self.replace_range_with(start, end, vec![vec![node]]);
        self.set_caret(Pos::new(line, start.col + 1));
        Did::Changed
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
                let head = sel.head;
                if head.col > 0 {
                    let line = text.line(head.line);
                    let char_before = line
                        .get(head.col - 1)
                        .and_then(crate::structure::text::as_char);
                    let char_after = line.get(head.col).and_then(crate::structure::text::as_char);
                    if let (Some(before_c), Some(after_c)) = (char_before, char_after) {
                        if matching_bracket(before_c) == Some(after_c) {
                            return (
                                Pos::new(head.line, head.col - 1),
                                Pos::new(head.line, head.col + 1),
                                Vec::new(),
                            );
                        }
                    }
                }
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

    /// Alt+↑ / Alt+↓: 選択行（または現在の行）を上下の行と入れ替えます。
    pub fn move_lines_vertical(&mut self, down: bool) -> Did {
        if self.touches_absent() || self.has_inside() {
            return Did::Nothing;
        }
        if self.cursors.is_empty() {
            return Did::Nothing;
        }
        let total_lines = self.document.text.line_count();
        let mut min_line = total_lines;
        let mut max_line = 0;
        for c in &self.cursors {
            let s = c.start();
            let e = c.end();
            min_line = min_line.min(s.line);
            if e.line > s.line && e.col == 0 {
                max_line = max_line.max(e.line.saturating_sub(1));
            } else {
                max_line = max_line.max(e.line);
            }
        }
        if min_line > max_line || max_line >= total_lines {
            return Did::Nothing;
        }

        if !down {
            // Alt+Up: 上の行と入れ替え
            if min_line == 0 {
                return Did::Nothing;
            }
            let swap_target = min_line - 1;
            let mut new_rows: Vec<Row> = Vec::with_capacity(max_line - swap_target + 1);
            for l in min_line..=max_line {
                new_rows.push(self.document.text.line(l).to_vec());
            }
            new_rows.push(self.document.text.line(swap_target).to_vec());

            self.record(Step::Other);
            let from = Pos::new(swap_target, 0);
            let to = Pos::new(max_line, self.document.text.line_len(max_line));
            let at = self.document.text.remove(from, to);
            self.document.text.insert(at, new_rows);
            self.mark_lines_modified(swap_target, max_line, max_line);

            for cursor in &mut self.cursors {
                if cursor.inside.is_none() {
                    if cursor.sel.head.line >= min_line && cursor.sel.head.line <= max_line {
                        cursor.sel.head.line -= 1;
                    } else if cursor.sel.head.line == swap_target {
                        cursor.sel.head.line = max_line;
                    }
                    if cursor.sel.anchor.line >= min_line && cursor.sel.anchor.line <= max_line {
                        cursor.sel.anchor.line -= 1;
                    } else if cursor.sel.anchor.line == swap_target {
                        cursor.sel.anchor.line = max_line;
                    }
                }
            }
        } else {
            // Alt+Down: 下の行と入れ替え
            if max_line + 1 >= total_lines {
                return Did::Nothing;
            }
            let swap_target = max_line + 1;
            let mut new_rows: Vec<Row> = Vec::with_capacity(swap_target - min_line + 1);
            new_rows.push(self.document.text.line(swap_target).to_vec());
            for l in min_line..=max_line {
                new_rows.push(self.document.text.line(l).to_vec());
            }

            self.record(Step::Other);
            let from = Pos::new(min_line, 0);
            let to = Pos::new(swap_target, self.document.text.line_len(swap_target));
            let at = self.document.text.remove(from, to);
            self.document.text.insert(at, new_rows);
            self.mark_lines_modified(min_line, swap_target, swap_target);

            for cursor in &mut self.cursors {
                if cursor.inside.is_none() {
                    if cursor.sel.head.line >= min_line && cursor.sel.head.line <= max_line {
                        cursor.sel.head.line += 1;
                    } else if cursor.sel.head.line == swap_target {
                        cursor.sel.head.line = min_line;
                    }
                    if cursor.sel.anchor.line >= min_line && cursor.sel.anchor.line <= max_line {
                        cursor.sel.anchor.line += 1;
                    } else if cursor.sel.anchor.line == swap_target {
                        cursor.sel.anchor.line = min_line;
                    }
                }
            }
        }
        Did::Changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::model::Document;
    use crate::structure::text::Text;

    fn make_editor(initial: &str) -> Editor {
        let mut doc = Document::default();
        let rows = if initial.is_empty() {
            vec![Vec::new()]
        } else {
            nodes_of(initial)
        };
        doc.load(Text::from_lines(rows));
        Editor {
            document: doc,
            cursors: vec![UnifiedCursor::caret(Pos::default())],
        }
    }

    fn text_of(editor: &Editor) -> String {
        editor
            .document
            .text
            .line(0)
            .iter()
            .map(|n| crate::structure::text::as_char(n).unwrap_or(' '))
            .collect()
    }

    #[test]
    fn auto_brackets_insert_pair_and_place_caret_in_between() {
        let mut editor = make_editor("");
        editor.insert_text("{");
        assert_eq!(text_of(&editor), "{}");
        assert_eq!(editor.primary().head, Pos::new(0, 1));

        editor.insert_text("abc");
        assert_eq!(text_of(&editor), "{abc}");
        assert_eq!(editor.primary().head, Pos::new(0, 4));

        // overtype closing bracket
        editor.insert_text("}");
        assert_eq!(text_of(&editor), "{abc}");
        assert_eq!(editor.primary().head, Pos::new(0, 5));
    }

    #[test]
    fn auto_brackets_pair_deletion_with_backspace() {
        let mut editor = make_editor("");
        editor.insert_text("(");
        assert_eq!(text_of(&editor), "()");
        assert_eq!(editor.primary().head, Pos::new(0, 1));

        editor.backspace();
        assert_eq!(text_of(&editor), "");
        assert_eq!(editor.primary().head, Pos::new(0, 0));
    }

    #[test]
    fn auto_brackets_wrap_selection() {
        let mut editor = make_editor("hello");
        editor.cursors = vec![UnifiedCursor::range(Pos::new(0, 0), Pos::new(0, 5))];
        editor.insert_text("「");
        assert_eq!(text_of(&editor), "「hello」");
        assert_eq!(editor.primary(), Sel::range(Pos::new(0, 1), Pos::new(0, 6)));
    }

    #[test]
    fn auto_brackets_2char_pair_from_ime() {
        let mut editor = make_editor("");
        editor.insert_text("【】");
        assert_eq!(text_of(&editor), "【】");
        assert_eq!(editor.primary().head, Pos::new(0, 1)); // caret in middle

        let mut editor2 = make_editor("");
        editor2.insert_text("『』");
        assert_eq!(text_of(&editor2), "『』");
        assert_eq!(editor2.primary().head, Pos::new(0, 1)); // caret in middle
    }

    #[test]
    fn word_selection_multilingual() {
        let mut editor =
            make_editor("function calculate税込み価格_total(金額) { return 金額 * 1.1; }");

        // English identifier
        editor.select_word_at(Pos::new(0, 2));
        assert_eq!(editor.primary(), Sel::range(Pos::new(0, 0), Pos::new(0, 8))); // "function"

        // Boundary click right after "function" (col 8, on whitespace)
        editor.select_word_at(Pos::new(0, 8));
        assert_eq!(editor.primary(), Sel::range(Pos::new(0, 0), Pos::new(0, 8))); // prefers "function"

        // Japanese Kanji
        editor.select_word_at(Pos::new(0, 21));
        assert_eq!(
            editor.primary(),
            Sel::range(Pos::new(0, 21), Pos::new(0, 23))
        ); // "価格"

        // Katakana with long vowel 'ー' and middle dot '・'
        let mut katakana_editor = make_editor("プレーンテキスト と ユーザー・インターフェース");
        katakana_editor.select_word_at(Pos::new(0, 3)); // 'ー' or 'ン' in "プレーンテキスト"
        assert_eq!(
            katakana_editor.primary(),
            Sel::range(Pos::new(0, 0), Pos::new(0, 8))
        ); // "プレーンテキスト"

        katakana_editor.select_word_at(Pos::new(0, 15)); // '・' in "ユーザー・インターフェース"
        assert_eq!(
            katakana_editor.primary(),
            Sel::range(Pos::new(0, 11), Pos::new(0, 24))
        ); // "ユーザー・インターフェース"

        // Whole line selection (Triple click)
        katakana_editor.select_line_at(Pos::new(0, 5));
        assert_eq!(
            katakana_editor.primary(),
            Sel::range(Pos::new(0, 0), Pos::new(0, 24))
        );
    }

    #[test]
    fn shift_up_down_extends_selection() {
        let mut doc = Document::default();
        doc.load(Text::from_lines(nodes_of(
            "first line\nsecond line\nthird line",
        )));
        let mut editor = Editor {
            document: doc,
            cursors: vec![UnifiedCursor::caret(Pos::new(1, 6))], // in "second line" at ' '
        };

        // Shift + Down -> expands to line 2
        editor.move_v(true, true);
        assert_eq!(editor.primary(), Sel::range(Pos::new(1, 6), Pos::new(2, 6)));

        // Shift + Up -> shrinks back
        editor.move_v(false, true);
        assert_eq!(editor.primary(), Sel::range(Pos::new(1, 6), Pos::new(1, 6)));

        // Shift + Up -> expands to line 0
        editor.move_v(false, true);
        assert_eq!(editor.primary(), Sel::range(Pos::new(1, 6), Pos::new(0, 6)));
    }

    #[test]
    fn alt_up_down_moves_lines() {
        let mut doc = Document::default();
        doc.load(Text::from_lines(nodes_of("Line 1\nLine 2\nLine 3")));
        let mut editor = Editor {
            document: doc,
            cursors: vec![UnifiedCursor::caret(Pos::new(1, 2))], // in Line 2
        };

        // Alt+Up moves Line 2 up to line 0
        editor.move_lines_vertical(false);
        let line0: String = editor.document.text.line(0).iter().filter_map(crate::structure::text::as_char).collect();
        let line1: String = editor.document.text.line(1).iter().filter_map(crate::structure::text::as_char).collect();
        let line2: String = editor.document.text.line(2).iter().filter_map(crate::structure::text::as_char).collect();
        assert_eq!(line0, "Line 2");
        assert_eq!(line1, "Line 1");
        assert_eq!(line2, "Line 3");
        assert_eq!(editor.primary().head, Pos::new(0, 2));

        // Alt+Down moves Line 2 back down to line 1
        editor.move_lines_vertical(true);
        let line0: String = editor.document.text.line(0).iter().filter_map(crate::structure::text::as_char).collect();
        let line1: String = editor.document.text.line(1).iter().filter_map(crate::structure::text::as_char).collect();
        let line2: String = editor.document.text.line(2).iter().filter_map(crate::structure::text::as_char).collect();
        assert_eq!(line0, "Line 1");
        assert_eq!(line1, "Line 2");
        assert_eq!(line2, "Line 3");
        assert_eq!(editor.primary().head, Pos::new(1, 2));
    }

    #[test]
    fn overwrite_mode_replaces_character() {
        let mut editor = make_editor("abcdef");
        editor.set_caret(Pos::new(0, 2)); // at 'c'
        editor.insert_text_with_mode("X", true); // overwrite 'c' with 'X'
        assert_eq!(text_of(&editor), "abXdef");
        assert_eq!(editor.primary().head, Pos::new(0, 3));
    }
}
