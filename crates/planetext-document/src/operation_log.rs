/// 履歴が持つステップ数の上限。それより古いものは忘れる。
pub(crate) const HISTORY_LIMIT: usize = 1000;

/// 履歴の 1 かたまりに入る 1 つの置き換えの逆: `from` から `inserted` 行を
/// 取り除き、`removed` を戻すと元に戻る。
#[derive(Debug)]
pub(crate) struct Edit {
    pub(crate) from: usize,
    pub(crate) removed: Vec<String>,
    pub(crate) inserted: usize,
}

/// 履歴の 1 ステップ。同じ `group` の置き換えが続く間は 1 つにつながるので、
/// 「すべて置換」も入力の 1 操作も、1 回の元に戻すで全部戻る。
#[derive(Debug)]
pub(crate) struct Step {
    pub(crate) group: u64,
    pub(crate) edits: Vec<Edit>,
    /// 編集前後のキャレットなどの控え。frontend が渡す不透明な文字列で、
    /// こちらは中身を解釈しない。
    pub(crate) before: String,
    pub(crate) after: String,
}

#[derive(Default)]
pub(crate) struct OperationLog {
    pub(crate) undo: Vec<Step>,
    pub(crate) redo: Vec<Step>,
    pub(crate) saved_undo_len: usize,
}

impl OperationLog {
    pub(crate) fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.saved_undo_len = 0;
    }

    pub(crate) fn is_clean(&self) -> bool {
        self.undo.len() == self.saved_undo_len
    }
}
