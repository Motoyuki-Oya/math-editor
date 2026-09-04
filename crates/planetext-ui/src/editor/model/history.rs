//! 編集のグループ化: 文書の本体が持つ元に戻す履歴の 1 ステップが、どこからどこ
//! までかを決める。文書のスナップショットはもう持たず、巻き戻し自体は本体が行う。
//!
//! 入力された文字の連なりと、1 操作にまとめられた編集（ショートカットの展開、
//! すべて置換）は同じグループ番号を受け取り、本体側で 1 ステップにつながる。

use super::Editor;
use crate::structure::text::LineChange;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// 入力された文字。その前のステップと結合します。
    Typing,
    Other,
}

#[derive(Clone, Debug)]
pub(crate) struct Recorder {
    /// 現在のグループ。新しいステップで増える。
    pub(crate) group: u64,
    pub(crate) last: Step,
    /// いくつかの編集が 1 ステップとして行われている間に設定されます。
    pub(crate) grouping: bool,
    /// そのステップが書き込まれると、残りがそれに結合されるように設定されます。
    pub(crate) grouped: bool,
    /// いま開いたグループの、編集前のキャレットの控え。回収されるまで持つ。
    pub(crate) before: Option<String>,
}

impl Default for Recorder {
    fn default() -> Self {
        Self {
            group: 0,
            last: Step::Other,
            grouping: false,
            grouped: false,
            before: None,
        }
    }
}

impl Recorder {
    /// 書き込まれているステップが終了するため、次のコマンドが独自のコマンドを開始します。キャレットを移動すると、同様に入力作業が省略されます。
    pub(super) fn cut(&mut self) {
        self.last = Step::Other;
    }
}

/// 文書の本体へ届ける、たまった編集のひとかたまり。
pub struct Flush {
    /// 本体の履歴で 1 ステップになる番号。同じ番号が続く間はつながる。
    pub group: u64,
    /// グループの最初の編集の前のキャレットの控え。続きの編集では空。
    pub before: String,
    /// いまのキャレットの控え。
    pub after: String,
    pub changes: Vec<LineChange>,
}

impl Editor {
    /// すべての「編集」が履歴の 1 ステップになります。入力したテキストを構造に変換するには複数の編集が必要で、元に戻すには、途中で構築された構造ではなく、入力された内容を元に戻す必要があります。
    pub fn one_step(&mut self, edits: impl FnOnce(&mut Editor)) {
        let was_grouping = self.recorder.grouping;
        self.recorder.grouping = true;
        edits(self);
        self.recorder.grouping = was_grouping;
        if !was_grouping {
            self.recorder.grouped = false;
        }
        self.recorder.cut();
    }

    /// 前のステップと結合しない限り、新しいグループを開いてキャレットを控える。
    pub(super) fn record(&mut self, step: Step) {
        let join =
            self.recorder.grouped || (step == Step::Typing && self.recorder.last == Step::Typing);
        self.recorder.last = step;
        if join {
            return;
        }
        self.recorder.grouped = self.recorder.grouping;
        self.recorder.group += 1;
        self.recorder.before = Some(self.state_string());
    }

    /// たまった編集を渡す。何も変わっていなければ `None`。
    pub fn take_flush(&mut self) -> Option<Flush> {
        let changes = self.text.take_changes();
        if changes.is_empty() {
            return None;
        }
        Some(Flush {
            group: self.recorder.group,
            before: self.recorder.before.take().unwrap_or_default(),
            after: self.state_string(),
            changes,
        })
    }

    /// 文書の本体が巻き戻ったのに合わせる: `touched_from` から先の手元の行を
    /// 捨てて届き直しを待ち、控えられていたキャレットへ戻る。
    pub fn apply_restored(&mut self, state: &str, touched_from: usize, line_count: usize) {
        self.text.reset_from(touched_from, line_count);
        self.restore_state(state);
        self.recorder.cut();
    }

    /// キャレットと選択の控え。文書の本体に預けるだけの不透明な文字列。
    pub(super) fn state_string(&self) -> String {
        self.cursors
            .iter()
            .map(|selection| {
                let sel = selection.sel;
                let base = format!(
                    "{}.{}-{}.{}",
                    sel.anchor.line, sel.anchor.col, sel.head.line, sel.head.col
                );
                let Some(cursor) = &selection.inside else {
                    return base;
                };
                let path = cursor
                    .path
                    .iter()
                    .map(|(node, slot)| format!("{node}.{slot}"))
                    .collect::<Vec<_>>()
                    .join(",");
                format!("{base}@{path}@{}@{}", cursor.index, cursor.anchor)
            })
            .collect::<Vec<_>>()
            .join(";")
    }

    pub fn restore_state(&mut self, state: &str) {
        use crate::structure::ast::Cursor;
        use crate::structure::text::{Pos, Sel};
        let mut cursors: Vec<super::UnifiedCursor> = state
            .split(';')
            .filter_map(|part| {
                let mut fields = part.split('@');
                let (anchor, head) = fields.next()?.split_once('-')?;
                let parse = |s: &str| -> Option<Pos> {
                    let (line, col) = s.split_once('.')?;
                    Some(Pos::new(line.parse().ok()?, col.parse().ok()?))
                };
                let sel = Sel {
                    anchor: self.text.clamp(parse(anchor)?),
                    head: self.text.clamp(parse(head)?),
                };
                let Some(path) = fields.next() else {
                    return Some(super::UnifiedCursor { sel, inside: None });
                };
                let path = path
                    .split(',')
                    .filter(|part| !part.is_empty())
                    .map(|part| {
                        let (node, slot) = part.split_once('.')?;
                        Some((node.parse().ok()?, slot.parse().ok()?))
                    })
                    .collect::<Option<Vec<_>>>()?;
                let index = fields.next()?.parse().ok()?;
                let cursor_anchor = fields.next()?.parse().ok()?;
                Some(super::UnifiedCursor {
                    sel,
                    inside: Some(Cursor {
                        path,
                        index,
                        anchor: cursor_anchor,
                    }),
                })
            })
            .collect();
        if cursors.is_empty() {
            cursors.push(super::UnifiedCursor::caret(Pos::default()));
        }
        self.cursors = cursors;
    }
}
