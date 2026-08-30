use super::Editor;
use crate::structure::ast::{Cursor, Node};
use crate::structure::text::{as_char, char_kind, is_word, CharKind, Pos, Sel, Text};

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(inline_js = r#"
let segmenter = null;
try {
    if (typeof Intl !== 'undefined' && Intl.Segmenter) {
        segmenter = new Intl.Segmenter(undefined, { granularity: 'word' });
    }
} catch (e) {
    segmenter = null;
}

export function segment_word_at_js(text, char_index) {
    if (!segmenter) return null;
    try {
        const chars = Array.from(text);
        if (char_index >= chars.length) return null;

        const isKatakana = (c) => /[\u30A0-\u30FF\u31F0-\u31FF\uFF65-\uFF9F\u30FC\u30FB]/.test(c);
        const isIdentChar = (c) => /[a-zA-Z0-9_]/.test(c) || /[\uFF10-\uFF19\uFF21-\uFF3A\uFF41-\uFF5A]/.test(c);

        // 1. カタカナ連続（長音符「ー」や中黒「・」を含む）を最優先でひと続きに選択
        if (isKatakana(chars[char_index])) {
            let start = char_index;
            let end = char_index + 1;
            while (start > 0 && isKatakana(chars[start - 1])) {
                start--;
            }
            while (end < chars.length && isKatakana(chars[end])) {
                end++;
            }
            return [start, end];
        }

        // 2. 英数・アンダースコア識別子の連続を最優先で選択
        if (isIdentChar(chars[char_index])) {
            let start = char_index;
            let end = char_index + 1;
            while (start > 0 && isIdentChar(chars[start - 1])) {
                start--;
            }
            while (end < chars.length && isIdentChar(chars[end])) {
                end++;
            }
            return [start, end];
        }

        // 3. 形態素解析 (Intl.Segmenter) による単語境界判定（漢字＋送り仮名等）
        const segments = segmenter.segment(text);
        let cur_idx = 0;
        let prev_seg = null;
        let target_seg = null;

        for (const seg of segments) {
            const char_len = Array.from(seg.segment).length;
            const start = cur_idx;
            const end = start + char_len;
            const item = { start, end, isWord: seg.isWordLike, segment: seg.segment };
            if (char_index >= start && char_index < end) {
                target_seg = item;
                break;
            }
            prev_seg = item;
            cur_idx = end;
        }

        if (target_seg) {
            if (!target_seg.isWord && prev_seg && prev_seg.isWord && char_index === target_seg.start) {
                target_seg = prev_seg;
            }
            let start = target_seg.start;
            let end = target_seg.end;
            if (isKatakana(chars[start])) {
                while (start > 0 && isKatakana(chars[start - 1])) {
                    start--;
                }
                while (end < chars.length && isKatakana(chars[end])) {
                    end++;
                }
            } else if (isIdentChar(chars[start])) {
                while (start > 0 && isIdentChar(chars[start - 1])) {
                    start--;
                }
                while (end < chars.length && isIdentChar(chars[end])) {
                    end++;
                }
            }
            return [start, end];
        }
    } catch (e) {
        return null;
    }
    return null;
}
"#)]
extern "C" {
    fn segment_word_at_js(text: &str, char_index: usize) -> wasm_bindgen::JsValue;
}

/// 形態素解析（Intl.Segmenter）を利用して指定位置の単語境界（start_char, end_char）を計算します。
pub fn segment_word(text: &str, char_index: usize) -> Option<(usize, usize)> {
    #[cfg(target_arch = "wasm32")]
    {
        let val = segment_word_at_js(text, char_index);
        if val.is_array() {
            let arr = js_sys::Array::from(&val);
            if arr.length() == 2 {
                let start = arr.get(0).as_f64()? as usize;
                let end = arr.get(1).as_f64()? as usize;
                return Some((start, end));
            }
        }
    }
    let _ = (text, char_index);
    None
}

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
}

impl UnifiedCursor {
    pub fn caret(at: Pos) -> Self {
        Self {
            sel: Sel::caret(at),
            inside: None,
        }
    }

    pub fn range(from: Pos, to: Pos) -> Self {
        Self {
            sel: Sel::range(from, to),
            inside: None,
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

pub fn merge_cursors(cursors: &mut Vec<UnifiedCursor>) {
    if cursors.is_empty() {
        return;
    }
    let primary = cursors.last().expect("at least one cursor").clone();
    cursors.sort_by_key(|cursor| {
        (
            cursor.start(),
            cursor.inside.as_ref().map(|inside| inside.path.clone()),
            cursor.end(),
        )
    });
    let mut merged: Vec<UnifiedCursor> = Vec::with_capacity(cursors.len());
    for cursor in std::mem::take(cursors) {
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
    *cursors = merged;
}

impl Editor {
    /// 選択内容がソートされ、重複がない状態が維持されるため、入力によって同じ編集が 2 回適用されることはありません。
    pub fn merge_sels(&mut self) {
        merge_cursors(&mut self.cursors);
    }

    /// 指定した位置の単語（英数字、識別子、漢字・カタカナ・ひらがな連続等）を選択します。
    pub fn select_word_at(&mut self, at: Pos) -> super::Did {
        self.leave_structure();
        let sel = word_at(&self.document.text, at).unwrap_or_else(|| {
            let line_len = self.document.text.line_len(at.line);
            let next_col = (at.col + 1).min(line_len);
            Sel::range(at, Pos::new(at.line, next_col))
        });
        self.cursors = vec![UnifiedCursor { sel, inside: None }];
        super::Did::Moved
    }

    /// 指定した位置の行全体を選択します。
    pub fn select_line_at(&mut self, at: Pos) -> super::Did {
        self.leave_structure();
        let line_len = self.document.text.line_len(at.line);
        let sel = Sel::range(Pos::new(at.line, 0), Pos::new(at.line, line_len));
        self.cursors = vec![UnifiedCursor { sel, inside: None }];
        super::Did::Moved
    }
}

pub fn word_at(text: &Text, at: Pos) -> Option<Sel> {
    let line = text.line(at.line);
    if line.is_empty() {
        return None;
    }

    // 文字列とNode列のインデックス対応表を作成
    let mut s = String::new();
    let mut col_map = Vec::new();
    for (col, node) in line.iter().enumerate() {
        if let Some(c) = as_char(node) {
            col_map.push(col);
            s.push(c);
        }
    }

    if !s.is_empty() {
        let target_char_idx = if at.col >= col_map.len() {
            col_map.len().saturating_sub(1)
        } else {
            at.col.min(col_map.len().saturating_sub(1))
        };

        // 形態素解析 (Intl.Segmenter) による単語境界判定
        if let Some((start_idx, end_idx)) = segment_word(&s, target_char_idx) {
            let start_col = col_map.get(start_idx).copied().unwrap_or(0);
            let end_col = if end_idx < col_map.len() {
                col_map[end_idx]
            } else {
                line.len()
            };
            return Some(Sel::range(
                Pos::new(at.line, start_col),
                Pos::new(at.line, end_col),
            ));
        }
    }

    // フォールバック: 文字種（CharKind）による境界判定
    let col = if at.col >= line.len() {
        line.len().saturating_sub(1)
    } else if at.col > 0
        && line
            .get(at.col)
            .and_then(as_char)
            .is_some_and(|c| c.is_whitespace() || char_kind(c) == CharKind::Punctuation)
    {
        if line.get(at.col - 1).and_then(as_char).is_some_and(is_word) {
            at.col - 1
        } else if at.col + 1 < line.len()
            && line.get(at.col + 1).and_then(as_char).is_some_and(is_word)
        {
            at.col + 1
        } else {
            at.col
        }
    } else {
        at.col
    };

    let target_char = line.get(col).and_then(as_char)?;
    let kind = char_kind(target_char);
    if kind == CharKind::Whitespace {
        let mut start = col;
        while start > 0
            && line
                .get(start - 1)
                .and_then(as_char)
                .is_some_and(|c| c.is_whitespace())
        {
            start -= 1;
        }
        let mut end = col + 1;
        while end < line.len()
            && line
                .get(end)
                .and_then(as_char)
                .is_some_and(|c| c.is_whitespace())
        {
            end += 1;
        }
        return Some(Sel::range(Pos::new(at.line, start), Pos::new(at.line, end)));
    }

    let mut start = col;
    while start > 0 {
        let prev_c = line.get(start - 1).and_then(as_char);
        if prev_c.is_some_and(|c| char_kind(c) == kind) {
            start -= 1;
        } else {
            break;
        }
    }
    let mut end = col + 1;
    while end < line.len() {
        let next_c = line.get(end).and_then(as_char);
        if next_c.is_some_and(|c| char_kind(c) == kind) {
            end += 1;
        } else {
            break;
        }
    }
    Some(Sel::range(Pos::new(at.line, start), Pos::new(at.line, end)))
}

/// 既に選択した場所をスキップして、 `from` からアイテムを探します。
pub fn find_after(text: &Text, needle: &[Node], from: Pos, taken: &[Pos]) -> Option<Sel> {
    if needle.is_empty() {
        return None;
    }
    let total_lines = text.line_count();
    let mut passes = Vec::with_capacity(2);
    if from == Pos::default() {
        passes.push(0..total_lines);
    } else {
        passes.push(from.line..total_lines);
        passes.push(0..from.line + 1);
    }
    for (pass_idx, range) in passes.into_iter().enumerate() {
        for line in range {
            if text.is_absent(line) {
                continue;
            }
            let items = text.line(line);
            if items.len() < needle.len() {
                continue;
            }
            let start_col = if pass_idx == 0 && line == from.line {
                from.col
            } else {
                0
            };
            let end_col_limit = if pass_idx == 1 && line == from.line {
                from.col.min(items.len().saturating_sub(needle.len()))
            } else {
                items.len().saturating_sub(needle.len())
            };
            for col in start_col..=end_col_limit {
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
    }
    None
}
