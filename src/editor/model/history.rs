//! The undo history: snapshots of the document, and how steps join.

use super::Editor;
use crate::structure::ast::Cursor;
use crate::structure::text::{Sel, Text};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Step {
    /// Typed characters, which join with the step before them.
    Typing,
    Other,
}

#[derive(Clone)]
struct Snapshot {
    text: Text,
    sels: Vec<Sel>,
    inside: Option<Cursor>,
}

/// What has been done, held as whole snapshots. Only [`Editor`] writes here:
/// the history knows when steps join, the editor knows what a step is.
pub(super) struct History {
    past: Vec<Snapshot>,
    future: Vec<Snapshot>,
    last: Step,
    /// Set while several edits are being made as one step of the history.
    grouping: bool,
    /// Set once that step has been written, so the rest join it.
    grouped: bool,
}

impl Default for History {
    fn default() -> Self {
        Self {
            past: Vec::new(),
            future: Vec::new(),
            last: Step::Other,
            grouping: false,
            grouped: false,
        }
    }
}

impl History {
    /// Forgets everything, when another document takes the editor over.
    pub(super) fn clear(&mut self) {
        self.past.clear();
        self.future.clear();
        self.last = Step::Other;
    }

    /// Ends the step being written, so the next command starts its own.
    /// Moving the caret cuts typing apart the same way.
    pub(super) fn cut(&mut self) {
        self.last = Step::Other;
    }
}

impl Editor {
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            text: self.text.clone(),
            sels: self.sels.clone(),
            inside: self.inside.clone(),
        }
    }

    /// Makes everything `edits` does one step of the history. Turning typed
    /// text into a structure takes several edits, and undoing it has to give
    /// back what was typed rather than the half-built structure in between.
    pub fn one_step(&mut self, edits: impl FnOnce(&mut Editor)) {
        let was_grouping = self.history.grouping;
        self.history.grouping = true;
        edits(self);
        self.history.grouping = was_grouping;
        if !was_grouping {
            self.history.grouped = false;
        }
        self.history.cut();
    }

    /// Writes the step about to be taken into the history, unless it joins
    /// the one before it.
    pub(super) fn record(&mut self, step: Step) {
        let join =
            self.history.grouped || (step == Step::Typing && self.history.last == Step::Typing);
        self.history.last = step;
        self.history.future.clear();
        if join {
            return;
        }
        self.history.grouped = self.history.grouping;
        let snapshot = self.snapshot();
        self.history.past.push(snapshot);
        if self.history.past.len() > crate::settings::history_limit() {
            self.history.past.remove(0);
        }
    }

    pub fn undo(&mut self) -> bool {
        let Some(previous) = self.history.past.pop() else {
            return false;
        };
        let now = self.snapshot();
        self.history.future.push(now);
        self.restore(previous);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.history.future.pop() else {
            return false;
        };
        let now = self.snapshot();
        self.history.past.push(now);
        self.restore(next);
        true
    }

    fn restore(&mut self, snapshot: Snapshot) {
        self.text = snapshot.text;
        self.sels = snapshot.sels;
        self.inside = snapshot.inside;
        self.history.cut();
    }
}
