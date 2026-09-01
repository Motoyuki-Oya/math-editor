use super::cursor::{find_after, word_at, UnifiedCursor};
use super::nested::Inside;
use super::{Did, Editor};
use crate::structure::ast::Row;
use crate::structure::text::{Pos, Sel, Text};

impl Editor {
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
            if extend {
                self.leave_structure();
            } else {
                self.move_vertical_cursors(down);
            }
        }
        self.map_sels(extend, |text, head| {
            let target_line_idx = if down {
                head.line + 1
            } else {
                head.line.checked_sub(1).unwrap_or(head.line)
            };
            let line = target_line_idx.min(text.line_count().saturating_sub(1));
            let col = if line != head.line {
                target_col_in_aligned(text.line(head.line), text.line(line), head.col)
            } else {
                head.col
            };
            text.clamp(Pos::new(line, col))
        });
        Did::Moved
    }

    /// ページ単位（PageUp / PageDown）の移動
    pub fn move_page(&mut self, down: bool, extend: bool) -> Did {
        if self.has_inside() {
            if extend {
                self.leave_structure();
            } else {
                self.move_vertical_cursors(down);
            }
        }
        self.map_sels(extend, |text, head| {
            let line = if down {
                head.line + 20
            } else {
                head.line.saturating_sub(20)
            };
            text.clamp(Pos::new(
                line.min(text.line_count().saturating_sub(1)),
                head.col,
            ))
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

    pub(super) fn map_sels(&mut self, extend: bool, step: impl Fn(&Text, Pos) -> Pos) {
        let Editor { document, cursors } = self;
        document.recorder.cut();
        for sel in cursors.iter_mut().filter(|cursor| cursor.inside.is_none()) {
            // 他のエディタと同様に、Shift を使用せずに選択範囲を折りたたむと、近くの端が維持されます。
            let from = if extend || sel.is_caret() {
                sel.head
            } else {
                sel.head.min(sel.anchor).max(sel.start())
            };
            let head = step(&document.text, from);
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
            .map(|sel| UnifiedCursor { sel, inside: None })
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
        });
        true
    }
}

pub fn before(text: &Text, at: Pos) -> Pos {
    if at.col > 0 {
        Pos::new(
            at.line,
            crate::structure::text::character_before(text.line(at.line), at.col),
        )
    } else if at.line > 0 {
        Pos::new(at.line - 1, text.line_len(at.line - 1))
    } else {
        at
    }
}

pub fn after(text: &Text, at: Pos) -> Pos {
    if at.col < text.line_len(at.line) {
        Pos::new(
            at.line,
            crate::structure::text::character_after(text.line(at.line), at.col),
        )
    } else if at.line + 1 < text.line_count() {
        Pos::new(at.line + 1, 0)
    } else {
        at
    }
}

/// Markdownテーブル（`|`）やTab区切り（`\t`）の行間で上下移動する際、同じセル・同じパイプ位置を基準にターゲット列を算出します。
fn target_col_in_aligned(
    cur_line: &[crate::structure::ast::Node],
    target_line: &[crate::structure::ast::Node],
    cur_col: usize,
) -> usize {
    // 1. Markdown テーブル（'|' を含む行同士）
    let cur_pipes: Vec<usize> = cur_line
        .iter()
        .enumerate()
        .filter_map(|(i, n)| (crate::structure::text::as_char(n) == Some('|')).then_some(i))
        .collect();
    let target_pipes: Vec<usize> = target_line
        .iter()
        .enumerate()
        .filter_map(|(i, n)| (crate::structure::text::as_char(n) == Some('|')).then_some(i))
        .collect();

    if !cur_pipes.is_empty() && !target_pipes.is_empty() {
        let pipe_idx = cur_pipes.iter().filter(|&&pos| cur_col > pos).count();
        if pipe_idx == 0 {
            let first_target_pipe = target_pipes[0];
            return cur_col.min(first_target_pipe);
        }
        if pipe_idx >= cur_pipes.len() {
            let cur_last_pipe = cur_pipes[cur_pipes.len() - 1];
            let offset_after_pipe = cur_col - (cur_last_pipe + 1);
            let target_last_pipe = target_pipes[target_pipes.len() - 1];
            let target_len = target_line.len();
            return (target_last_pipe + 1 + offset_after_pipe).min(target_len);
        }
        let cur_prev_pipe = cur_pipes[pipe_idx - 1];
        let offset = cur_col - (cur_prev_pipe + 1);
        let target_pipe_idx = (pipe_idx - 1).min(target_pipes.len() - 1);
        let target_prev_pipe = target_pipes[target_pipe_idx];
        let target_next_pipe = if target_pipe_idx + 1 < target_pipes.len() {
            target_pipes[target_pipe_idx + 1]
        } else {
            target_line.len()
        };
        return (target_prev_pipe + 1 + offset).min(target_next_pipe);
    }

    // 2. Tab 整列（'\t' を含む行同士）
    let cur_tabs: Vec<usize> = cur_line
        .iter()
        .enumerate()
        .filter_map(|(i, n)| (crate::structure::text::as_char(n) == Some('\t')).then_some(i))
        .collect();
    let target_tabs: Vec<usize> = target_line
        .iter()
        .enumerate()
        .filter_map(|(i, n)| (crate::structure::text::as_char(n) == Some('\t')).then_some(i))
        .collect();

    if !cur_tabs.is_empty() && !target_tabs.is_empty() {
        let tab_idx = cur_tabs.iter().filter(|&&pos| cur_col > pos).count();
        if tab_idx == 0 {
            let first_target_tab = target_tabs[0];
            return cur_col.min(first_target_tab);
        }
        if tab_idx >= cur_tabs.len() {
            let cur_last_tab = cur_tabs[cur_tabs.len() - 1];
            let offset = cur_col - (cur_last_tab + 1);
            let target_last_tab = target_tabs[target_tabs.len() - 1];
            return (target_last_tab + 1 + offset).min(target_line.len());
        }
        let cur_prev_tab = cur_tabs[tab_idx - 1];
        let offset = cur_col - (cur_prev_tab + 1);
        let target_tab_idx = (tab_idx - 1).min(target_tabs.len() - 1);
        let target_prev_tab = target_tabs[target_tab_idx];
        let target_next_tab = if target_tab_idx + 1 < target_tabs.len() {
            target_tabs[target_tab_idx + 1]
        } else {
            target_line.len()
        };
        return (target_prev_tab + 1 + offset).min(target_next_tab);
    }

    // 通常の行
    cur_col
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::structure::ast::Node;

    fn to_nodes(s: &str) -> Vec<Node> {
        s.chars().map(Node::char).collect()
    }

    #[test]
    fn table_vertical_movement_aligns_by_pipe() {
        let line3 = to_nodes("| aaaaaaaaaaaaaaaaa | bdddddddddddddddddddddddddddddddddd |");
        let line4 = to_nodes("| ddddddddddddddddd | cccc |");

        // line4の末尾（最後のパイプの後ろ）から上へ移動した際、line3の末尾（最後のパイプの後ろ）に到達する
        let col = target_col_in_aligned(&line4, &line3, line4.len());
        assert_eq!(col, line3.len());

        // line4の第2セル内から上へ移動した際、line3の第2セルの同オフセットに到達する
        let line4_cell2 = 22;
        let col2 = target_col_in_aligned(&line4, &line3, line4_cell2);
        assert_eq!(col2, 22);
    }
}
