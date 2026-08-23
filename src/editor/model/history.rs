//! 編集のグループ化: 文書の本体が持つ元に戻す履歴の 1 ステップが、どこからどこ
//! までかを決める。文書のスナップショットはもう持たず、巻き戻し自体は本体が行う。
//!
//! 入力された文字の連なりと、1 操作にまとめられた編集（ショートカットの展開、
//! すべて置換）は同じグループ番号を受け取り、本体側で 1 ステップにつながる。

use super::Editor;
use crate::structure::text::LineChange;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Step {
    /// 入力された文字。その前のステップと結合します。
    Typing,
    Other,
}

pub(super) struct Recorder {
    /// 現在のグループ。新しいステップで増える。
    group: u64,
    last: Step,
    /// いくつかの編集が 1 ステップとして行われている間に設定されます。
    grouping: bool,
    /// そのステップが書き込まれると、残りがそれに結合されるように設定されます。
    grouped: bool,
    /// いま開いたグループの、編集前のキャレットの控え。回収されるまで持つ。
    before: Option<String>,
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
        self.cursor = None;
        self.transient_structure = None;
        self.restore_state(state);
        self.recorder.cut();
    }

    /// キャレットと選択の控え。文書の本体に預けるだけの不透明な文字列。
    pub(super) fn state_string(&self) -> String {
        self.sels
            .iter()
            .map(|sel| {
                format!(
                    "{}.{}-{}.{}",
                    sel.anchor.line, sel.anchor.col, sel.head.line, sel.head.col
                )
            })
            .collect::<Vec<_>>()
            .join(";")
    }

    fn restore_state(&mut self, state: &str) {
        use crate::structure::text::{Pos, Sel};
        let mut sels: Vec<Sel> = state
            .split(';')
            .filter_map(|part| {
                let (anchor, head) = part.split_once('-')?;
                let parse = |s: &str| -> Option<Pos> {
                    let (line, col) = s.split_once('.')?;
                    Some(Pos::new(line.parse().ok()?, col.parse().ok()?))
                };
                Some(Sel {
                    anchor: self.text.clamp(parse(anchor)?),
                    head: self.text.clamp(parse(head)?),
                })
            })
            .collect();
        if sels.is_empty() {
            sels.push(Sel::caret(Pos::default()));
        }
        self.sels = sels;
    }
}
