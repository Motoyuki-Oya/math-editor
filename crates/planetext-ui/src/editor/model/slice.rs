//! 各ペイン（ビュー）ごとに 1 つ存在するスライスモデル。
//! 表示窓周辺の行の写し（Text）、固有のキャレット・選択範囲、および編集履歴を管理する。
//! 文書の真実（ファイルモデル）は持たず、いつでも破棄して取り寄せ直せる。

use std::collections::BTreeSet;

use super::cursor::UnifiedCursor;
use super::history::Recorder;
use crate::structure::text::{Pos, Sel, Text};

#[derive(Default)]
pub struct SliceModel {
    #[allow(dead_code)]
    pub pane: usize,
    #[allow(dead_code)]
    pub doc_id: usize,
    pub text: Text,
    #[allow(dead_code)]
    pub cursors: Vec<UnifiedCursor>,
    pub recorder: Recorder,
    pub modified_lines: BTreeSet<usize>,
    pub file_bytes: Option<usize>,
    pub counting: bool,
}

impl SliceModel {
    pub fn new(pane: usize, doc_id: usize) -> Self {
        Self {
            pane,
            doc_id,
            text: Text::default(),
            cursors: vec![UnifiedCursor::caret(Pos::default())],
            recorder: Recorder::default(),
            modified_lines: BTreeSet::new(),
            file_bytes: None,
            counting: false,
        }
    }

    pub fn is_counting(&self) -> bool {
        self.counting
    }

    pub fn text(&self) -> &Text {
        &self.text
    }

    #[allow(dead_code)]
    pub fn text_mut(&mut self) -> &mut Text {
        &mut self.text
    }

    #[allow(dead_code)]
    pub fn file_bytes(&self) -> Option<usize> {
        self.file_bytes
    }

    pub fn set_file_bytes(&mut self, bytes: Option<usize>) {
        self.file_bytes = bytes;
    }

    pub fn modified_lines(&self) -> Vec<usize> {
        self.modified_lines.iter().copied().collect()
    }

    pub fn clear_modified(&mut self) {
        self.modified_lines.clear();
    }

    pub fn set_modified_lines(&mut self, lines: Vec<usize>) {
        self.modified_lines = lines.into_iter().collect();
    }

    pub fn mark_lines_modified(&mut self, from_line: usize, to_line: usize, end_line: usize) {
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
        self.counting = false;
    }

    pub fn load_sparse(&mut self, line_count: Option<usize>) {
        const INITIAL_PENDING_LINES: usize = 1_000;
        match line_count {
            Some(count) => {
                self.load(Text::pending(count.max(1)));
                self.counting = false;
            }
            None => {
                self.load(Text::pending(INITIAL_PENDING_LINES));
                self.counting = true;
            }
        }
    }

    pub fn load_pending(&mut self, line_count: usize) {
        if line_count == 0 {
            self.load_sparse(None);
        } else {
            self.load_sparse(Some(line_count));
            self.counting = true;
        }
    }

    pub fn resize_pending(&mut self, line_count: usize) {
        self.text.resize_pending(line_count);
        self.counting = false;
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
        let end = from + lines.len();
        if self.counting && end > self.text.line_count() {
            self.text.resize_pending(end);
        }
        for (offset, line) in lines.into_iter().enumerate() {
            self.text.fill_line(from + offset, line);
        }
    }

    #[allow(dead_code)]
    pub fn stats(&self) -> (usize, usize) {
        self.text.stats()
    }

    #[allow(dead_code)]
    pub fn primary(&self) -> Sel {
        self.cursors.last().expect("at least one cursor").sel
    }

    #[allow(dead_code)]
    pub fn primary_cursor(&self) -> &UnifiedCursor {
        self.cursors.last().expect("at least one cursor")
    }

    #[allow(dead_code)]
    pub fn cursors(&self) -> &[UnifiedCursor] {
        &self.cursors
    }

    #[allow(dead_code)]
    pub fn sels(&self) -> Vec<Sel> {
        self.cursors
            .iter()
            .filter(|cursor| cursor.inside.is_none())
            .map(|cursor| cursor.sel)
            .collect()
    }

    /// 外部（他ペインや文書エンジン）での行範囲置換をこのスライスへ反映する。
    pub fn apply_external_edit(&mut self, from: usize, to: usize, lines: &[String]) {
        let delta = lines.len() as isize - (to as isize - from as isize);
        let source_lines: Vec<crate::structure::text::SourceLine> = lines
            .iter()
            .map(|s| crate::structure::text::SourceLine::Plain(s.clone()))
            .collect();
        self.text.replace_external(from, to, source_lines);
        if !lines.is_empty() {
            self.mark_lines_modified(from, to, from + lines.len() - 1);
        } else {
            let removed_lines = to.saturating_sub(from);
            let mut next_modified = std::collections::BTreeSet::new();
            for &line in &self.modified_lines {
                if line < from {
                    next_modified.insert(line);
                } else if line >= to {
                    let shifted = (line as isize - removed_lines as isize).max(0) as usize;
                    next_modified.insert(shifted);
                }
            }
            self.modified_lines = next_modified;
        }

        if delta != 0 {
            for cursor in &mut self.cursors {
                if cursor.sel.anchor.line >= to {
                    cursor.sel.anchor.line =
                        (cursor.sel.anchor.line as isize + delta).max(0) as usize;
                } else if cursor.sel.anchor.line >= from {
                    cursor.sel.anchor.line = (from + lines.len().saturating_sub(1))
                        .min(self.text.line_count().saturating_sub(1));
                }
                if cursor.sel.head.line >= to {
                    cursor.sel.head.line =
                        (cursor.sel.head.line as isize + delta).max(0) as usize;
                } else if cursor.sel.head.line >= from {
                    cursor.sel.head.line = (from + lines.len().saturating_sub(1))
                        .min(self.text.line_count().saturating_sub(1));
                }
                cursor.sel.anchor = self.text.clamp(cursor.sel.anchor);
                cursor.sel.head = self.text.clamp(cursor.sel.head);
            }
        }
    }
}
