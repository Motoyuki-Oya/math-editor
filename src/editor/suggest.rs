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

/// 単語がプログラミング識別子（CamelCase, snake_case, 定数, 長い単語）であるか判定します。
pub fn is_code_identifier(word: &str) -> bool {
    if word.len() < 3 {
        return false;
    }
    // snake_case
    if word.contains('_') {
        return true;
    }
    // CamelCase / PascalCase
    let mut has_lower = false;
    let mut has_upper = false;
    for (i, c) in word.chars().enumerate() {
        if c.is_lowercase() {
            has_lower = true;
        } else if c.is_uppercase() {
            if i > 0 && has_lower {
                return true; // camelCase
            }
            has_upper = true;
        }
    }
    if has_upper && has_lower {
        return true; // PascalCase
    }
    // ALL_CAPS (定数など)
    if has_upper && !has_lower && word.len() >= 3 {
        return true;
    }
    // 7文字以上の長い識別子
    word.len() >= 7
}

/// 指定された行とキャレット位置から、直前の単語プレフィックスを抽出します。
pub fn extract_prefix(line_text: &str, col: usize) -> Option<String> {
    let chars: Vec<char> = line_text.chars().collect();
    let col = col.min(chars.len());
    if col == 0 {
        return None;
    }

    // キャレットの直後に英数字やアンダースコアが存在する場合（単語の途中: 例 "pub|lic"）は補完を抑制
    if col < chars.len() && (chars[col].is_alphanumeric() || chars[col] == '_') {
        return None;
    }

    // LaTeX コマンド（\frac 等）のプレフィックス判定
    let mut start = col;
    while start > 0
        && (chars[start - 1].is_alphanumeric()
            || chars[start - 1] == '_'
            || chars[start - 1] == '\\')
    {
        start -= 1;
        if start < col && chars[start] == '\\' {
            break;
        }
    }

    let prefix_chars = &chars[start..col];
    if prefix_chars.is_empty() {
        return None;
    }

    let prefix: String = prefix_chars.iter().collect();
    if prefix.starts_with('\\') && prefix.chars().count() >= 2 {
        return Some(prefix);
    }

    if !prefix_chars[0].is_alphabetic() && prefix_chars[0] != '_' {
        return None;
    }

    // 通常の単語は2文字以上で候補を出す
    if prefix.chars().count() >= 2 {
        Some(prefix)
    } else {
        None
    }
}

/// ドキュメントからプログラミング識別子（単語）を収集します。
pub fn collect_buffer_words(text: &Text, max_lines_scan: usize) -> HashSet<String> {
    let mut words = HashSet::new();
    let count = text.line_count().min(max_lines_scan);
    for line_idx in 0..count {
        let row: Row = text.line(line_idx).to_vec();
        let plain_line = plain::row(&row);
        for part in plain_line.split(|c: char| !c.is_alphanumeric() && c != '_') {
            if is_code_identifier(part) {
                words.insert(part.to_string());
            }
        }
    }
    words
}

/// Markdown ドキュメントにおいて、指定行がコードブロック内（``` で囲まれた範囲）にあるか判定し、指定されている言語名を返します。
pub fn markdown_code_block_lang(text: &Text, target_line: usize) -> Option<Option<String>> {
    let mut in_block = false;
    let mut block_lang = None;
    for line_idx in 0..=target_line {
        let row = text.line(line_idx);
        let plain_line = plain::row(&row.to_vec());
        let trimmed = plain_line.trim_start();
        if trimmed.starts_with("```") {
            if in_block {
                in_block = false;
                block_lang = None;
            } else {
                in_block = true;
                let lang_name = trimmed.trim_start_matches('`').trim().to_string();
                block_lang = if lang_name.is_empty() {
                    None
                } else {
                    Some(lang_name)
                };
            }
        }
    }
    if in_block {
        Some(block_lang)
    } else {
        None
    }
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

    let is_markdown = lang.is_some_and(|l| l.name == "Markdown");

    // 1. LaTeX コマンドの補完 (\frac, \alpha 等)
    if prefix.starts_with('\\') {
        if let Some(latex_def) = crate::syntax::for_name("LaTeX") {
            for word in latex_def.keywords.iter().chain(&latex_def.builtins) {
                if word.to_lowercase().starts_with(&prefix_lower) && word.len() > prefix.len() {
                    candidates.push(word.clone());
                }
            }
        }
    } else if !is_markdown {
        // 通常のコード言語: 言語キーワード・型・組み込み辞書からの検索
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

        // バッファ内識別子からの検索
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
        assert_eq!(extract_prefix("formula: \\fr", 12), Some("\\fr".into()));
        // 単語の途中（pub|lic）では None
        assert_eq!(extract_prefix("public", 3), None);
        assert_eq!(extract_prefix("user_name", 4), None);
    }

    #[test]
    fn test_is_code_identifier() {
        assert!(is_code_identifier("user_name"));
        assert!(is_code_identifier("calculateTotalPrice"));
        assert!(is_code_identifier("UserConfig"));
        assert!(is_code_identifier("MAX_BUFFER_SIZE"));
        assert!(!is_code_identifier("the"));
        assert!(!is_code_identifier("and"));
        assert!(!is_code_identifier("this"));
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
        let langs = built_in_languages();
        let rust = langs.iter().find(|l| l.name == "Rust").unwrap();
        let ghost = find_suggestion(0, 3, "cal", Some(rust), Some(&words))
            .expect("Should suggest calculateTotalPrice");
        assert_eq!(ghost.prefix, "cal");
        assert_eq!(ghost.suffix, "culateTotalPrice");
        assert_eq!(ghost.full, "calculateTotalPrice");
    }

    #[test]
    fn test_markdown_latex_suggestion() {
        let langs = built_in_languages();
        let md = langs.iter().find(|l| l.name == "Markdown").unwrap();
        let ghost = find_suggestion(0, 5, "$\\fr", Some(md), None).expect("Should suggest \\frac");
        assert_eq!(ghost.prefix, "\\fr");
        assert_eq!(ghost.suffix, "ac");
        assert_eq!(ghost.full, "\\frac");
    }
}
