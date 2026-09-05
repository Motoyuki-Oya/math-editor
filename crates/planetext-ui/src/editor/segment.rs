//! 単語分割の抽象界面とデフォルト実装。
//! モデル層から特定の GUI / ブラウザ API（Intl.Segmenter 等）への直接依存を排除し、
//! テスト環境や将来のネイティブ表示環境（GPUI等）でも動作可能にします。

use std::sync::RwLock;
use unicode_segmentation::UnicodeSegmentation;

/// 単語分割プロバイダのトレイト。
/// 与えられたテキストの指定文字位置（UTF-8 code point index）における単語の開始・終了文字位置を返します。
pub trait WordSegmenter: Send + Sync {
    fn segment_word(&self, text: &str, char_index: usize) -> Option<(usize, usize)>;
}

/// カタカナ判定
fn is_katakana(c: char) -> bool {
    matches!(
        c,
        '\u{30A0}'..='\u{30FF}' | '\u{31F0}'..='\u{31FF}' | '\u{FF65}'..='\u{FF9F}'
    )
}

/// 識別子文字判定（英数字、アンダースコア、全角英数）
fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || c == '_'
        || matches!(c, '\u{FF10}'..='\u{FF19}' | '\u{FF21}'..='\u{FF3A}' | '\u{FF41}'..='\u{FF5A}')
}

/// 純 Rust によるデフォルトの単語分割実装。
/// `unicode-segmentation` の UAX #29 単語境界と、日本語カタカナ・英数識別子のルールを組み合わせます。
#[derive(Debug, Default)]
pub struct DefaultWordSegmenter;

impl WordSegmenter for DefaultWordSegmenter {
    fn segment_word(&self, text: &str, char_index: usize) -> Option<(usize, usize)> {
        let chars: Vec<char> = text.chars().collect();
        if char_index >= chars.len() {
            return None;
        }

        let target_char = chars[char_index];

        // 1. カタカナ連続（長音符「ー」や中黒「・」を含む）を最優先でひと続きに選択
        if is_katakana(target_char) {
            let mut start = char_index;
            let mut end = char_index + 1;
            while start > 0 && is_katakana(chars[start - 1]) {
                start -= 1;
            }
            while end < chars.len() && is_katakana(chars[end]) {
                end += 1;
            }
            return Some((start, end));
        }

        // 2. 英数・アンダースコア識別子の連続を最優先で選択
        if is_ident_char(target_char) {
            let mut start = char_index;
            let mut end = char_index + 1;
            while start > 0 && is_ident_char(chars[start - 1]) {
                start -= 1;
            }
            while end < chars.len() && is_ident_char(chars[end]) {
                end += 1;
            }
            return Some((start, end));
        }

        // 3. 漢字の連続
        if crate::structure::text::char_kind(target_char) == crate::structure::text::CharKind::Kanji
        {
            let mut start = char_index;
            let mut end = char_index + 1;
            while start > 0
                && crate::structure::text::char_kind(chars[start - 1])
                    == crate::structure::text::CharKind::Kanji
            {
                start -= 1;
            }
            while end < chars.len()
                && crate::structure::text::char_kind(chars[end])
                    == crate::structure::text::CharKind::Kanji
            {
                end += 1;
            }
            return Some((start, end));
        }

        // 4. ひらがなの連続
        if crate::structure::text::char_kind(target_char)
            == crate::structure::text::CharKind::Hiragana
        {
            let mut start = char_index;
            let mut end = char_index + 1;
            while start > 0
                && crate::structure::text::char_kind(chars[start - 1])
                    == crate::structure::text::CharKind::Hiragana
            {
                start -= 1;
            }
            while end < chars.len()
                && crate::structure::text::char_kind(chars[end])
                    == crate::structure::text::CharKind::Hiragana
            {
                end += 1;
            }
            return Some((start, end));
        }

        // 5. unicode-segmentation による単語境界判定
        let mut cur_char_idx = 0;
        let mut prev_word = None;
        let mut target_word = None;

        for word in text.split_word_bounds() {
            let word_char_len = word.chars().count();
            let start = cur_char_idx;
            let end = start + word_char_len;
            let is_word_like = word.chars().any(|c| c.is_alphanumeric());

            if char_index >= start && char_index < end {
                target_word = Some((start, end, is_word_like));
                break;
            }

            if is_word_like {
                prev_word = Some((start, end));
            }
            cur_char_idx = end;
        }

        if let Some((start, end, is_word_like)) = target_word {
            if !is_word_like && char_index == start {
                if let Some((p_start, p_end)) = prev_word {
                    return Some((p_start, p_end));
                }
            }
            return Some((start, end));
        }

        None
    }
}

static SEGMENTER: RwLock<Option<Box<dyn WordSegmenter>>> = RwLock::new(None);

/// プラットフォーム固有の単語分割プロバイダ（Web の Intl.Segmenter 等）を登録します。
#[allow(dead_code)]
pub fn set_word_segmenter(segmenter: Box<dyn WordSegmenter>) {
    if let Ok(mut lock) = SEGMENTER.write() {
        *lock = Some(segmenter);
    }
}

/// 指定文字位置の単語境界（start_char, end_char）を計算します。
/// 登録されたプロバイダがあればそれを優先し、なければデフォルトの純 Rust 実装を使用します。
pub fn segment_word(text: &str, char_index: usize) -> Option<(usize, usize)> {
    if let Ok(lock) = SEGMENTER.read() {
        if let Some(ref s) = *lock {
            return s.segment_word(text, char_index);
        }
    }
    DefaultWordSegmenter.segment_word(text, char_index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_segmenter_handles_katakana() {
        let text = "これはテストです";
        // 'テ' は char_index 3
        let res = segment_word(text, 3);
        assert_eq!(res, Some((3, 6))); // "テスト"
    }

    #[test]
    fn default_segmenter_handles_identifier() {
        let text = "let mut foo_bar123 = 42;";
        // 'f' は char_index 8
        let res = segment_word(text, 8);
        assert_eq!(res, Some((8, 18))); // "foo_bar123"
    }

    #[test]
    fn default_segmenter_handles_ascii_words() {
        let text = "hello world";
        let res = segment_word(text, 1);
        assert_eq!(res, Some((0, 5))); // "hello"
    }
}
