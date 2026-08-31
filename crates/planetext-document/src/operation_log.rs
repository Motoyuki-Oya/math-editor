use crate::edit_buffers::{EditBuffers, EditRange};
use crate::source::{FileEncoding, LineEnding};

pub(crate) const HISTORY_LIMIT: usize = 1000;

#[derive(Debug)]
pub(crate) struct Edit {
    pub(crate) from: usize,
    pub(crate) removed: EditRange,
    pub(crate) inserted: usize,
}

#[derive(Debug)]
pub(crate) struct Step {
    pub(crate) group: u64,
    pub(crate) edits: Vec<Edit>,
    pub(crate) before: String,
    pub(crate) after: String,
}

#[derive(Default)]
pub(crate) struct OperationLog {
    pub(crate) undo: Vec<Step>,
    pub(crate) redo: Vec<Step>,
    pub(crate) saved_undo_len: usize,
    pub(crate) delete_buffers: EditBuffers,
}

impl OperationLog {
    pub(crate) fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.saved_undo_len = 0;
        self.delete_buffers = EditBuffers::default();
    }
    pub(crate) fn is_clean(&self) -> bool {
        self.undo.len() == self.saved_undo_len
    }
    pub(crate) fn append_deleted(
        &mut self,
        lines: &[String],
        encoding: FileEncoding,
        line_ending: LineEnding,
    ) -> EditRange {
        self.delete_buffers
            .append_lines(lines, encoding, line_ending)
            .0
    }
    pub(crate) fn read_deleted(&self, range: EditRange) -> Vec<String> {
        self.delete_buffers.read_lines(range)
    }
}
