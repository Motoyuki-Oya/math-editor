use super::Editor;
use crate::structure::ast::{Cursor, Node};
use crate::structure::text::{as_char, Pos, Sel, Text};

/// `to` までのテキストが `end` で終わるテキストに置き換えられると、`pos` が終了します。
pub fn shifted(pos: Pos, to: Pos, end: Pos) -> Pos {
    if pos <= to {
        // 編集が飲み込んだものはすべて最後に残ります。
        return end;
    }
    let line = (pos.line + end.line).saturating_sub(to.line);
    if pos.line == to.line {
        Pos::new(line, pos.col - to.col + end.col)
    } else {
        Pos::new(line, pos.col)
    }
}

/// 本文と入れ子構造を同じ配列で持つ選択。`sel` は文書上の行と構造Nodeの
/// 位置を示し、`inside` があればそこから入れ子Rowまでの絶対パスを示す。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnifiedCursor {
    pub sel: Sel,
    pub inside: Option<Cursor>,
    /// 本文トリガーが今回構造を作る文書行内の位置。
    pub transient_structure: Option<usize>,
}

impl UnifiedCursor {
    pub fn caret(at: Pos) -> Self {
        Self {
            sel: Sel::caret(at),
            inside: None,
            transient_structure: None,
        }
    }

    pub fn range(from: Pos, to: Pos) -> Self {
        Self {
            sel: Sel::range(from, to),
            inside: None,
            transient_structure: None,
        }
    }
}

impl std::ops::Deref for UnifiedCursor {
    type Target = Sel;

    fn deref(&self) -> &Self::Target {
        &self.sel
    }
}

impl std::ops::DerefMut for UnifiedCursor {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.sel
    }
}

impl Editor {
    /// 選択内容がソートされ、重複がない状態が維持されるため、入力によって同じ編集が 2 回適用されることはありません。
    pub fn merge_sels(&mut self) {
        let primary = self.primary_cursor().clone();
        self.cursors.sort_by_key(|cursor| {
            (
                cursor.start(),
                cursor.inside.as_ref().map(|inside| inside.path.clone()),
                cursor.end(),
            )
        });
        let mut merged: Vec<UnifiedCursor> = Vec::with_capacity(self.cursors.len());
        for cursor in std::mem::take(&mut self.cursors) {
            match merged.last_mut() {
                Some(last)
                    if last.inside.is_none()
                        && cursor.inside.is_none()
                        && cursor.start() <= last.end() =>
                {
                    if cursor.end() > last.end() {
                        last.sel = Sel::range(last.start(), cursor.end());
                    }
                }
                Some(last) if *last == cursor => {}
                _ => merged.push(cursor),
            }
        }
        // 「プライマリ」がそれを意味し続けるように、フォーカスされた選択範囲は最後に残す。
        if let Some(index) = merged.iter().position(|cursor| {
            *cursor == primary
                || (cursor.inside.is_none()
                    && primary.inside.is_none()
                    && cursor.start() <= primary.start()
                    && primary.end() <= cursor.end())
        }) {
            let focused = merged.remove(index);
            merged.push(focused);
        }
        self.cursors = merged;
    }
}

pub fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

pub fn word_at(text: &Text, at: Pos) -> Option<Sel> {
    let line = text.line(at.line);
    let word = |col: usize| line.get(col).and_then(as_char).is_some_and(is_word);
    let mut start = at.col;
    while start > 0 && word(start - 1) {
        start -= 1;
    }
    let mut end = at.col;
    while word(end) {
        end += 1;
    }
    (start < end).then(|| Sel::range(Pos::new(at.line, start), Pos::new(at.line, end)))
}

/// 既に選択した場所をスキップして、 `from` からアイテムを探します。
pub fn find_after(text: &Text, needle: &[Node], from: Pos, taken: &[Pos]) -> Option<Sel> {
    let mut at = from;
    for _ in 0..text.line_count() + 1 {
        for line in at.line..text.line_count() {
            let items = text.line(line);
            let start_col = if line == at.line { at.col } else { 0 };
            for col in start_col..=items.len().saturating_sub(needle.len()) {
                if items.len() < needle.len() {
                    break;
                }
                if &items[col..col + needle.len()] == needle
                    && !taken.contains(&Pos::new(line, col))
                {
                    return Some(Sel::range(
                        Pos::new(line, col),
                        Pos::new(line, col + needle.len()),
                    ));
                }
            }
        }
        if at == Pos::default() {
            return None;
        }
        at = Pos::default();
    }
    None
}
