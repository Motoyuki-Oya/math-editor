//! インライン補完（ゴーストテキスト / Phantom Text）エンジン。
//!
//! キャレット直前の単語プレフィックス（例: "pu"）を検出し、言語キーワード辞書および
//! ドキュメント（バッファ）内に登場する識別子から最適な補完候補を導出します。

use std::collections::HashSet;

use crate::structure::ast::Row;
use crate::structure::plain;
use crate::structure::text::Text;
use crate::syntax::lang::LanguageDef;

pub use crate::syntax::GhostText;

/// 指定された行とキャレット位置から、直前の単語プレフィックスを抽出します。
pub fn extract_prefix(line_text: &str, col: usize) -> Option<String> {
    let chars: Vec<char> = line_text.chars().collect();
    let col = col.min(chars.len());
    if col == 0 {
        return None;
    }

    let mut start = col;
    while start > 0 && (chars[start - 1].is_alphanumeric() || chars[start - 1] == '_') {
        start -= 1;
    }

    let prefix_chars = &chars[start..col];
    if prefix_chars.is_empty() || !prefix_chars[0].is_alphabetic() && prefix_chars[0] != '_' {
        return None;
    }

    let prefix: String = prefix_chars.iter().collect();
    // 1文字だけの場合は誤爆やチラつきを防ぐため、2文字以上で候補を出す
    if prefix.chars().count() >= 2 {
        Some(prefix)
    } else {
        None
    }
}

/// ドキュメントから識別子（単語）を収集します。
pub fn collect_buffer_words(text: &Text, max_lines_scan: usize) -> HashSet<String> {
    let mut words = HashSet::new();
    let count = text.line_count().min(max_lines_scan);
    for line_idx in 0..count {
        let row: Row = text.line(line_idx).to_vec();
        let plain_line = plain::row(&row);
        for part in plain_line.split(|c: char| !c.is_alphanumeric() && c != '_') {
            if part.chars().count() >= 3
                && part
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphabetic() || c == '_')
            {
                words.insert(part.to_string());
            }
        }
    }
    words
}

/// 現在のキャレット位置に対してゴーストテキスト候補を検索します。
pub fn find_suggestion(
    line_idx: usize,
    col: usize,
    line_text: &str,
    lang: Option<&LanguageDef>,
    buffer_words: Option<&HashSet<String>>,
) -> Option<GhostText> {
    let prefix = extract_prefix(line_text, col)?;
    let prefix_lower = prefix.to_lowercase();

    let mut candidates: Vec<String> = Vec::new();

    // 1. 言語キーワード・型・組み込み辞書からの検索
    if let Some(def) = lang {
        for word in def
            .keywords
            .iter()
            .chain(&def.types)
            .chain(&def.builtins)
            .chain(&def.constants)
        {
            if word.to_lowercase().starts_with(&prefix_lower) && word.len() > prefix.len() {
                candidates.push(word.clone());
            }
        }
    }

    // 2. バッファ内識別子からの検索
    if let Some(words) = buffer_words {
        for word in words {
            if word.to_lowercase().starts_with(&prefix_lower)
                && word.len() > prefix.len()
                && !candidates.contains(word)
            {
                candidates.push(word.clone());
            }
        }
    }

    if candidates.is_empty() {
        return None;
    }

    // ソート基準:
    // ① プレフィックスと完全一致（大文字小文字の一致）を優先
    // ② 文字数が短いものを優先（よりシンプルな補完）
    // ③ アルファベット順
    candidates.sort_by(|a, b| {
        let a_exact = a.starts_with(&prefix);
        let b_exact = b.starts_with(&prefix);
        if a_exact != b_exact {
            return b_exact.cmp(&a_exact);
        }
        let len_cmp = a.len().cmp(&b.len());
        if len_cmp != std::cmp::Ordering::Equal {
            return len_cmp;
        }
        a.cmp(b)
    });

    let best = candidates.into_iter().next()?;
    let suffix = if best.starts_with(&prefix) {
        best[prefix.len()..].to_string()
    } else {
        // 大文字小文字が異なる場合
        let char_count = prefix.chars().count();
        best.chars().skip(char_count).collect()
    };

    if suffix.is_empty() {
        return None;
    }

    Some(GhostText {
        line: line_idx,
        col,
        prefix,
        suffix,
        full: best,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::lang::built_in_languages;

    #[test]
    fn test_extract_prefix() {
        assert_eq!(
            extract_prefix("let calculate = 1", 13),
            Some("calculate".into())
        );
        assert_eq!(extract_prefix("  pu", 4), Some("pu".into()));
        assert_eq!(extract_prefix("  p", 3), None); // 1文字はNone
        assert_eq!(extract_prefix("hello(pu", 8), Some("pu".into()));
    }

    #[test]
    fn test_kotlin_keyword_suggestion() {
        let langs = built_in_languages();
        let kt = langs.iter().find(|l| l.name == "Kotlin").unwrap();
        let ghost = find_suggestion(0, 2, "pu", Some(kt), None).expect("Should suggest public");
        assert_eq!(ghost.prefix, "pu");
        assert_eq!(ghost.suffix, "blic");
        assert_eq!(ghost.full, "public");
    }

    #[test]
    fn test_buffer_word_suggestion() {
        let mut words = HashSet::new();
        words.insert("calculateTotalPrice".into());
        let ghost = find_suggestion(0, 3, "cal", None, Some(&words))
            .expect("Should suggest calculateTotalPrice");
        assert_eq!(ghost.prefix, "cal");
        assert_eq!(ghost.suffix, "culateTotalPrice");
        assert_eq!(ghost.full, "calculateTotalPrice");
    }
}
