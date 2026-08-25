use super::cursor::{shifted, UnifiedCursor};
use super::history::Step;
use super::movement::{after, before};
use super::nested::Inside;
use super::{Did, Editor};
use crate::editor::clipboard::Clip;
use crate::structure::ast::{Cursor, Node, Row};
use crate::structure::text::{nodes_of, Pos, Sel, Text};

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
}
