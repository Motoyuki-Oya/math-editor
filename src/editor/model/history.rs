//! 元に戻す履歴: ドキュメントのスナップショット、およびステップの結合方法。

use super::Editor;
use crate::structure::ast::Cursor;
use crate::structure::text::{Sel, Text};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Step {
    /// 入力された文字。その前のステップと結合します。
    Typing,
    Other,
}

#[derive(Clone)]
struct Snapshot {
    text: Text,
    sels: Vec<Sel>,
    cursor: Option<Cursor>,
}

/// 実行内容。スナップショット全体として保持されます。 [`Editor`] だけがここに書き込みます。履歴はステップがいつ結合するかを知っており、エディタはステップが何であるかを知っています。
pub(super) struct History {
    past: Vec<Snapshot>,
    future: Vec<Snapshot>,
    last: Step,
    /// いくつかの編集が履歴の 1 ステップとして行われている間に設定されます。
    grouping: bool,
    /// そのステップが書き​​込まれると、残りがそれに結合されるように設定されます。
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
    /// 別のドキュメントがエディタを引き継ぐと、すべてが忘れられます。
    pub(super) fn clear(&mut self) {
        self.past.clear();
        self.future.clear();
        self.last = Step::Other;
    }

    /// 書き込まれているステップが終了するため、次のコマンドが独自のコマンドを開始します。キャレットを移動すると、同様に入力作業が省略されます。
    pub(super) fn cut(&mut self) {
        self.last = Step::Other;
    }
}

impl Editor {
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            text: self.text.clone(),
            sels: self.sels.clone(),
            cursor: self.cursor.clone(),
        }
    }

    /// すべての「編集」が履歴の 1 ステップになります。入力したテキストを構造に変換するには複数の編集が必要で、元に戻すには、途中で構築された構造ではなく、入力された内容を元に戻す必要があります。
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

    /// 前のステップと結合しない限り、履歴に取り込まれようとしているステップを書き込みます。
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
        if self.is_loading() {
            return false;
        }
        let Some(previous) = self.history.past.pop() else {
            return false;
        };
        let now = self.snapshot();
        self.history.future.push(now);
        self.restore(previous);
        true
    }

    pub fn redo(&mut self) -> bool {
        if self.is_loading() {
            return false;
        }
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
        self.cursor = snapshot.cursor;
        self.history.cut();
    }
}
