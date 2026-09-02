use crate::edit_buffers::{EditBuffers, EditRange};
use crate::source::{FileEncoding, LineEnding};

pub(crate) const HISTORY_LIMIT: usize = 1000;

/// 物理バイト座標と行座標を併せ持つ編集単位。
#[derive(Clone, Debug)]
pub(crate) struct Edit {
    /// 適用前（base_revision）の物理バイト開始位置
    pub(crate) from: usize,
    /// 適用前（base_revision）の物理バイト終了位置
    pub(crate) to: usize,
    /// 編集開始行番号
    pub(crate) from_line: usize,
    pub(crate) removed: EditRange,
    pub(crate) inserted: EditRange,
    pub(crate) removed_lines: usize,
    pub(crate) inserted_lines: usize,
}

use std::sync::Arc;

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) enum BulkOperation {
    AllLines {
        from_line: usize,
        to_line: usize,
        column: usize,
        delete: usize,
        insert: String,
    },
    ReplaceAll {
        from_line: usize,
        to_line: usize,
        query: String,
        replacement: String,
        case_sensitive: bool,
        pattern: Arc<regex::Regex>,
    },
}

#[derive(Clone, Debug)]
pub(crate) enum OperationKind {
    Splice(Vec<Edit>),
    Bulk(BulkOperation),
}

#[derive(Clone, Debug)]
pub(crate) struct Transaction {
    pub(crate) group: u64,
    pub(crate) base_revision: u64,
    pub(crate) revision: u64,
    pub(crate) kind: OperationKind,
    pub(crate) before: String,
    pub(crate) after: String,
}

impl Transaction {
    pub(crate) fn edits(&self) -> &[Edit] {
        match &self.kind {
            OperationKind::Splice(edits) => edits,
            OperationKind::Bulk(_) => &[],
        }
    }
}

/// 単一の追記専用操作ログ。
/// Undo / Redo は `head` カーソルの移動のみで表現され、ログの外に別の状態を作らない。
#[derive(Clone)]
pub(crate) struct OperationLog {
    pub(crate) transactions: Vec<Transaction>,
    pub(crate) head: usize,
    pub(crate) next_revision: u64,
    pub(crate) saved_revision: Option<u64>,
    pub(crate) retained_base: u64,
    pub(crate) base_revision: u64,
    pub(crate) buffers: EditBuffers,
}

impl Default for OperationLog {
    fn default() -> Self {
        Self {
            transactions: Vec::new(),
            head: 0,
            next_revision: 1,
            saved_revision: Some(0),
            retained_base: 0,
            base_revision: 0,
            buffers: EditBuffers::default(),
        }
    }
}

impl OperationLog {
    pub(crate) fn revision(&self) -> u64 {
        self.head
            .checked_sub(1)
            .and_then(|index| self.transactions.get(index))
            .map_or(self.base_revision, |tx| tx.revision)
    }

    pub(crate) fn memory_usage(&self) -> usize {
        self.buffers.len() + self.transactions.len() * std::mem::size_of::<Transaction>()
    }

    pub(crate) fn clear(&mut self) {
        self.transactions.clear();
        self.head = 0;
        self.base_revision = self.next_revision;
        self.next_revision = self.next_revision.saturating_add(1);
        self.retained_base = self.base_revision;
        self.saved_revision = Some(self.base_revision);
        self.buffers = EditBuffers::default();
    }

    pub(crate) fn mark_dirty_without_history(&mut self) {
        self.saved_revision = None;
    }

    pub(crate) fn mark_saved(&mut self) {
        self.saved_revision = Some(self.revision());
    }

    pub(crate) fn is_clean(&self) -> bool {
        self.saved_revision == Some(self.revision())
    }

    pub(crate) fn unsaved_transactions(&self) -> &[Transaction] {
        let saved = self.saved_revision.unwrap_or(0);
        let valid = &self.transactions[..self.head];
        let start = valid
            .iter()
            .position(|tx| tx.revision > saved)
            .unwrap_or(valid.len());
        &valid[start..]
    }

    pub(crate) fn append_transaction(
        &mut self,
        base_revision: u64,
        group: u64,
        edits: Vec<Edit>,
        before: &str,
        after: &str,
    ) {
        // Undo 状態で新しい編集が入った場合、Redo 枝を切り捨てる
        self.transactions.truncate(self.head);
        let revision = self.next_revision;
        self.next_revision = self.next_revision.saturating_add(1);
        self.transactions.push(Transaction {
            group,
            base_revision,
            revision,
            kind: OperationKind::Splice(edits),
            before: before.to_string(),
            after: after.to_string(),
        });
        self.head = self.transactions.len();

        if self.transactions.len() > HISTORY_LIMIT {
            self.transactions.remove(0);
            self.head = self.head.saturating_sub(1);
            self.retained_base = self
                .transactions
                .first()
                .map_or(self.next_revision, |tx| tx.base_revision);
            self.base_revision = self.retained_base;
        }
    }

    pub(crate) fn append_bulk_transaction(
        &mut self,
        base_revision: u64,
        group: u64,
        bulk: BulkOperation,
        before: &str,
        after: &str,
    ) {
        self.transactions.truncate(self.head);
        let revision = self.next_revision;
        self.next_revision = self.next_revision.saturating_add(1);
        self.transactions.push(Transaction {
            group,
            base_revision,
            revision,
            kind: OperationKind::Bulk(bulk),
            before: before.to_string(),
            after: after.to_string(),
        });
        self.head = self.transactions.len();

        if self.transactions.len() > HISTORY_LIMIT {
            self.transactions.remove(0);
            self.head = self.head.saturating_sub(1);
            self.retained_base = self
                .transactions
                .first()
                .map_or(self.next_revision, |tx| tx.base_revision);
            self.base_revision = self.retained_base;
        }
    }

    /// 直前のトランザクションと同じ group であれば edits をマージする
    pub(crate) fn append_or_merge_transaction(
        &mut self,
        base_revision: u64,
        group: u64,
        edit: Edit,
        before: &str,
        after: &str,
    ) {
        self.transactions.truncate(self.head);
        let current_rev = self.revision();
        if let Some(last) = self.transactions.last_mut() {
            if last.group == group && last.revision == current_rev {
                if let OperationKind::Splice(edits) = &mut last.kind {
                    edits.push(edit);
                    last.after = after.to_string();
                    return;
                }
            }
        }
        self.append_transaction(base_revision, group, vec![edit], before, after);
    }

    pub(crate) fn active_bulk_operations(&self) -> Vec<(&BulkOperation, u64)> {
        self.transactions[..self.head]
            .iter()
            .filter_map(|tx| match &tx.kind {
                OperationKind::Bulk(bulk) => Some((bulk, tx.revision)),
                OperationKind::Splice(_) => None,
            })
            .collect()
    }

    /// Undo: head を 1 つ戻し、巻き戻すべきトランザクションを返す
    pub(crate) fn undo_pop(&mut self) -> Option<&Transaction> {
        if self.head == 0 {
            return None;
        }
        self.head -= 1;
        self.transactions.get(self.head)
    }

    /// Redo: head を 1 つ進め、再適用すべきトランザクションを返す
    pub(crate) fn redo_pop(&mut self) -> Option<&Transaction> {
        if self.head >= self.transactions.len() {
            return None;
        }
        let tx = self.transactions.get(self.head);
        self.head += 1;
        tx
    }

    pub(crate) fn append_deleted(
        &mut self,
        lines: &[String],
        encoding: FileEncoding,
        line_ending: LineEnding,
    ) -> EditRange {
        self.buffers.append_lines(lines, encoding, line_ending).0
    }

    pub(crate) fn read_deleted(&self, range: EditRange) -> Vec<String> {
        self.buffers.read_lines(range)
    }

    /// 指定された base_revision が有効（追跡範囲内）か検証する
    pub(crate) fn validate_base(&self, revision: u64) -> Result<(), String> {
        if revision == self.revision() {
            return Ok(());
        }
        if revision < self.retained_base
            || revision >= self.next_revision
            || !self
                .transactions
                .iter()
                .any(|tx| tx.revision == revision || tx.base_revision == revision)
        {
            return Err("revision is discarded, future, or unknown".into());
        }
        Ok(())
    }

    fn active_transactions_after(&self, revision: u64) -> impl Iterator<Item = &Transaction> {
        self.transactions
            .iter()
            .take(self.head)
            .filter(move |tx| tx.base_revision >= revision || tx.revision > revision)
    }

    /// base_revision 上のバイト座標を、現在の head のバイト座標へ写像する。
    /// 削除された範囲に該当した場合はエラーを返す。
    pub(crate) fn map_point(&self, base_revision: u64, point: usize) -> Result<usize, String> {
        self.validate_base(base_revision)?;
        let mut value = point;
        for tx in self.active_transactions_after(base_revision) {
            let mut delta: isize = 0;
            let input = value;
            for edit in tx.edits() {
                let from = edit.from;
                let end = edit.to;
                if input < from {
                    break;
                }
                if input < end {
                    return Err("coordinate overlaps a removed range".into());
                }
                delta += edit.inserted.len as isize - edit.to.saturating_sub(edit.from) as isize;
            }
            value = if delta >= 0 {
                input.saturating_add(delta as usize)
            } else {
                input.saturating_sub((-delta) as usize)
            };
        }
        Ok(value)
    }

    /// base_revision 上の半開区間 [from, to) を、現在の head の物理区間へ写像する。
    /// 区間が編集範囲と重なっている場合はエラーを返す。
    pub(crate) fn map_range(
        &self,
        base_revision: u64,
        from: usize,
        to: usize,
    ) -> Result<(usize, usize), String> {
        if from > to {
            return Err("range is reversed".into());
        }
        if from == to {
            let point = self.map_point(base_revision, from)?;
            return Ok((point, point));
        }
        self.validate_base(base_revision)?;
        let mut start = from;
        let mut end = to;
        for tx in self.active_transactions_after(base_revision) {
            for edit in tx.edits() {
                if start < edit.to && edit.from < end {
                    return Err("range overlaps an edit".into());
                }
            }
            let shift = |boundary: usize, include_boundary: bool| {
                tx.edits()
                    .iter()
                    .take_while(|edit| {
                        edit.from < boundary || (include_boundary && edit.from == boundary)
                    })
                    .filter(|edit| edit.to <= boundary)
                    .map(|edit| {
                        edit.inserted.len as isize - edit.to.saturating_sub(edit.from) as isize
                    })
                    .sum::<isize>()
            };
            let start_delta = shift(start, true);
            let end_delta = shift(end, false);
            start = if start_delta >= 0 {
                start.saturating_add(start_delta as usize)
            } else {
                start.saturating_sub((-start_delta) as usize)
            };
            end = if end_delta >= 0 {
                end.saturating_add(end_delta as usize)
            } else {
                end.saturating_sub((-end_delta) as usize)
            };
        }
        Ok((start, end))
    }

    /// base_revision 上の行範囲 [from_line, to_line) を、現在の head の行範囲へ写像する。
    /// 区間が編集・削除された行と重なっている場合はエラーを返す。
    pub(crate) fn map_line_range(
        &self,
        base_revision: u64,
        from_line: usize,
        to_line: usize,
    ) -> Result<(usize, usize), String> {
        if from_line > to_line {
            return Err("range is reversed".into());
        }
        self.validate_base(base_revision)?;
        let mut cur_from = from_line;
        let mut cur_to = to_line;
        for tx in self.active_transactions_after(base_revision) {
            for edit in tx.edits() {
                let edit_start = edit.from_line;
                let edit_end = edit.from_line + edit.removed_lines;
                if cur_from < edit_end && edit_start < cur_to {
                    return Err("line range overlaps an edit".into());
                }
                let delta = edit.inserted_lines as isize - edit.removed_lines as isize;
                if edit_end <= cur_from {
                    cur_from = if delta >= 0 {
                        cur_from + delta as usize
                    } else {
                        cur_from.saturating_sub((-delta) as usize)
                    };
                }
                if edit_end <= cur_to {
                    cur_to = if delta >= 0 {
                        cur_to + delta as usize
                    } else {
                        cur_to.saturating_sub((-delta) as usize)
                    };
                }
            }
        }
        Ok((cur_from, cur_to))
    }
}
