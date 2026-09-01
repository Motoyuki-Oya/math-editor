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

/// ドキュメントの指定範囲からプログラミング識別子（単語）を収集します。
pub fn collect_buffer_words_range(text: &Text, range: std::ops::Range<usize>) -> HashSet<String> {
    let mut words = HashSet::new();
    let end = range.end.min(text.line_count());
    for line_idx in range.start..end {
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

/// ドキュメントからプログラミング識別子（単語）を収集します。
#[allow(dead_code)]
pub fn collect_buffer_words(text: &Text, max_lines_scan: usize) -> HashSet<String> {
    collect_buffer_words_range(text, 0..max_lines_scan)
}

/// Markdown ドキュメントにおいて、指定行がコードブロック内（``` で囲まれた範囲）にあるか判定し、指定されている言語名を返します。
pub fn markdown_code_block_lang(text: &Text, target_line: usize) -> Option<Option<String>> {
    let mut in_block = false;
    let mut block_lang = None;
    for line_idx in 0..=target_line {
        let row = text.line(line_idx);
        let plain_line = plain::row(row);
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

/// 漢字（CJK統合漢字）判定
pub fn is_kanji(c: char) -> bool {
    matches!(c, '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}')
}

/// キャレット直前が漢字であるか判定（単語の途中は除く）
pub fn is_kanji_preceding(nodes: &[crate::structure::ast::Node], col: usize) -> bool {
    let col = col.min(nodes.len());
    if col == 0 {
        return false;
    }
    if col < nodes.len() {
        if let crate::structure::ast::NodeKind::Char(c) = nodes[col].kind {
            if is_kanji(c) {
                return false;
            }
        }
    }
    if let crate::structure::ast::NodeKind::Char(c) = nodes[col - 1].kind {
        is_kanji(c)
    } else {
        false
    }
}

pub const COMMON_RUBY_DICT: &[(&str, &str)] = &[
    ("機能一覧", "きのういちらん"),
    ("日本語", "にほんご"),
    ("実験的", "じっけんてき"),
    ("構造化", "こうぞうか"),
    ("独擅場", "どくせんじょう"),
    ("独壇場", "どくだんじょう"),
    ("未曾有", "みぞう"),
    ("紆余曲折", "うよきょくせつ"),
    ("試金石", "しきんせき"),
    ("登竜門", "とうりゅうもん"),
    ("登龍門", "とうりゅうもん"),
    ("脆弱性", "ぜいじゃくせい"),
    ("逆鱗", "げきりん"),
    ("相殺", "そうさい"),
    ("稀薄", "きはく"),
    ("希薄", "きはく"),
    ("代替", "だいたい"),
    ("出納", "すいのう"),
    ("破綻", "はたん"),
    ("貼付", "ちょうふ"),
    ("漸次", "ぜんじ"),
    ("暫時", "ざんじ"),
    ("早急", "さっきゅう"),
    ("汎用", "はんよう"),
    ("凡例", "はんれい"),
    ("脆弱", "ぜいじゃく"),
    ("遵守", "じゅんしゅ"),
    ("順守", "じゅんしゅ"),
    ("重宝", "ちょうほう"),
    ("杜撰", "ずさん"),
    ("捏造", "ねつぞう"),
    ("忖度", "そんたく"),
    ("破天荒", "はてんこう"),
    ("辟易", "へきえき"),
    ("蹂躙", "じゅうりん"),
    ("憐憫", "れんびん"),
    ("杞憂", "きゆう"),
    ("暫定", "ざんてい"),
    ("瑕疵", "かし"),
    ("稟議", "りんぎ"),
    ("既出", "きしゅつ"),
    ("割愛", "かつあい"),
    ("委嘱", "いしょく"),
    ("軋轢", "あつれき"),
    ("示唆", "しさ"),
    ("拘泥", "こうでい"),
    ("躊躇", "ちゅうちょ"),
    ("逡巡", "しゅんじゅん"),
    ("矜持", "きょうじ"),
    ("挨拶", "あいさつ"),
    ("役務", "えきむ"),
    ("供託", "きょうたく"),
    ("頒布", "はんぷ"),
    ("疾病", "しっぺい"),
    ("贅沢", "ぜいたく"),
    ("貪欲", "どんよく"),
    ("煩雑", "はんざつ"),
    ("繁雑", "はんざつ"),
    ("煩瑣", "はんさ"),
    ("逼迫", "ひっぱく"),
    ("歪曲", "わいきょく"),
    ("補填", "ほてん"),
    ("遺憾", "いかん"),
    ("惹起", "じゃっき"),
    ("固執", "こしゅう"),
    ("敷衍", "ふえん"),
    ("吹聴", "ふいちょう"),
    ("押印", "おういん"),
    ("捺印", "なついん"),
    ("誤謬", "ごびゅう"),
    ("逝去", "せいきょ"),
    ("会釈", "えしゃく"),
    ("嘲笑", "ちょうしょう"),
    ("蔑視", "べっし"),
    ("傀儡", "かいらい"),
    ("齟齬", "そご"),
    ("隠蔽", "いんぺい"),
    ("翻弄", "ほんろう"),
    ("席巻", "せっけん"),
    ("狼狽", "ろうばい"),
    ("諮問", "しもん"),
    ("骨子", "こっし"),
    ("標榜", "ひょうぼう"),
    ("団欒", "だんらん"),
    ("破竹", "はちく"),
    ("生粋", "きっすい"),
    ("稀有", "けう"),
    ("希有", "けう"),
    ("稀代", "きだい"),
    ("希代", "きだい"),
    ("便宜", "べんぎ"),
    ("鳥瞰", "ちょうかん"),
    ("薫陶", "くんとう"),
    ("漸減", "ぜんげん"),
    ("漸増", "ぜんぞう"),
    ("概要", "がいよう"),
    ("特徴", "とくちょう"),
    ("機能", "きのう"),
    ("一覧", "いちらん"),
    ("漢字", "かんじ"),
    ("専用", "せんよう"),
    ("記法", "きほう"),
    ("必要", "ひつよう"),
    ("数式", "すうしき"),
    ("保存", "ほぞん"),
    ("確定", "かくてい"),
    ("編集", "へんしゅう"),
    ("設定", "せってい"),
    ("文字", "もじ"),
    ("流用", "りゅうよう"),
    ("補完", "ほかん"),
    ("整列", "せいれつ"),
    ("日本", "にほん"),
    ("作成", "さくせい"),
    ("変更", "へんこう"),
    ("確認", "かくにん"),
    ("実行", "じっこう"),
    ("検索", "けんさく"),
    ("置換", "ちかん"),
    ("新規", "しんき"),
    ("表示", "ひょうじ"),
];

/// ドキュメントの指定範囲から既存のルビ定義（漢字 -> 読み）を収集します。
pub fn collect_buffer_rubies_range(
    text: &Text,
    range: std::ops::Range<usize>,
) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let end = range.end.min(text.line_count());
    for line_idx in range.start..end {
        let row = text.line(line_idx);
        collect_rubies_from_row(row, &mut map);
    }
    map
}

/// ドキュメントから既存のルビ定義（漢字 -> 読み）を収集します。
#[allow(dead_code)]
pub fn collect_buffer_rubies(
    text: &Text,
    max_lines: usize,
) -> std::collections::HashMap<String, String> {
    collect_buffer_rubies_range(text, 0..max_lines)
}

fn collect_rubies_from_row(
    row: &[crate::structure::ast::Node],
    map: &mut std::collections::HashMap<String, String>,
) {
    use crate::structure::ast::NodeKind;
    for node in row {
        if !node.upper.is_empty() {
            let base_text = match &node.kind {
                NodeKind::Container(inner) => plain::row(inner),
                _ => plain::row(std::slice::from_ref(node)),
            };
            let reading = plain::row(&node.upper);
            if !base_text.is_empty() && !reading.is_empty() {
                map.insert(base_text, reading);
            }
        }
        for slot in 0..node.slot_count() {
            if let Some(child_row) = node.slot(slot) {
                collect_rubies_from_row(child_row, map);
            }
        }
    }
}

/// キャレット直前の漢字文字列からルビ読み候補を検索します。
pub fn extract_ruby_candidate_nodes(
    nodes: &[crate::structure::ast::Node],
    col: usize,
    buffer_rubies: Option<&std::collections::HashMap<String, String>>,
) -> Option<(String, String)> {
    use crate::structure::ast::NodeKind;
    let col = col.min(nodes.len());
    if col == 0 {
        return None;
    }
    if col < nodes.len() {
        if let NodeKind::Char(c) = nodes[col].kind {
            if is_kanji(c) {
                return None;
            }
        }
    }

    let mut start = col;
    while start > 0 {
        if let NodeKind::Char(c) = nodes[start - 1].kind {
            if is_kanji(c) {
                start -= 1;
                continue;
            }
        }
        break;
    }
    if start == col {
        return None;
    }

    let kanji_phrase: String = nodes[start..col]
        .iter()
        .filter_map(|n| {
            if let NodeKind::Char(c) = n.kind {
                Some(c)
            } else {
                None
            }
        })
        .collect();

    if kanji_phrase.is_empty() {
        return None;
    }

    let kanji_count = kanji_phrase.chars().count();

    // 1. バッファ内ルビ辞書から最長一致検索
    if let Some(rubies) = buffer_rubies {
        for len in (1..=kanji_count).rev() {
            let sub: String = kanji_phrase.chars().skip(kanji_count - len).collect();
            if let Some(reading) = rubies.get(&sub) {
                return Some((sub, reading.clone()));
            }
        }
    }

    // 2. 共通辞書から最長一致検索
    for len in (1..=kanji_count).rev() {
        let sub: String = kanji_phrase.chars().skip(kanji_count - len).collect();
        for &(dict_kanji, reading) in COMMON_RUBY_DICT {
            if dict_kanji == sub {
                return Some((sub.to_string(), reading.to_string()));
            }
        }
    }

    None
}

/// 現在のキャレット位置に対してゴーストテキスト候補を検索します。
pub fn find_suggestion(
    line_idx: usize,
    col: usize,
    nodes: &[crate::structure::ast::Node],
    line_text: &str,
    lang: Option<&LanguageDef>,
    buffer_words: Option<&HashSet<String>>,
    buffer_rubies: Option<&std::collections::HashMap<String, String>>,
) -> Option<GhostText> {
    let prefix = extract_prefix(line_text, col);
    let mut candidates: Vec<String> = Vec::new();

    if let Some(ref prefix) = prefix {
        let prefix_lower = prefix.to_lowercase();
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
    }

    if let (Some(prefix), false) = (prefix, candidates.is_empty()) {
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
            let char_count = prefix.chars().count();
            best.chars().skip(char_count).collect()
        };

        if !suffix.is_empty() {
            return Some(GhostText {
                line: line_idx,
                col,
                prefix,
                suffix,
                full: best,
                ruby: None,
            });
        }
    }

    // 2. 日本語漢字の構造化（ルビ）補完候補
    if let Some((kanji, reading)) = extract_ruby_candidate_nodes(nodes, col, buffer_rubies) {
        let kanji_count = kanji.chars().count();
        return Some(GhostText {
            line: line_idx,
            col,
            prefix: kanji.clone(),
            suffix: String::new(),
            full: format!("{kanji}（{reading}）"),
            ruby: Some((kanji_count, reading)),
        });
    }

    None
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

    fn to_nodes(text: &str) -> Vec<crate::structure::ast::Node> {
        text.chars()
            .map(crate::structure::ast::Node::char)
            .collect()
    }

    #[test]
    fn test_kotlin_keyword_suggestion() {
        let langs = built_in_languages();
        let kt = langs.iter().find(|l| l.name == "Kotlin").unwrap();
        let nodes = to_nodes("pu");
        let ghost = find_suggestion(0, 2, &nodes, "pu", Some(kt), None, None)
            .expect("Should suggest public");
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
        let nodes = to_nodes("cal");
        let ghost = find_suggestion(0, 3, &nodes, "cal", Some(rust), Some(&words), None)
            .expect("Should suggest calculateTotalPrice");
        assert_eq!(ghost.prefix, "cal");
        assert_eq!(ghost.suffix, "culateTotalPrice");
        assert_eq!(ghost.full, "calculateTotalPrice");
    }

    #[test]
    fn test_markdown_latex_suggestion() {
        let langs = built_in_languages();
        let md = langs.iter().find(|l| l.name == "Markdown").unwrap();
        let nodes = to_nodes("$\\fr");
        let ghost = find_suggestion(0, 5, &nodes, "$\\fr", Some(md), None, None)
            .expect("Should suggest \\frac");
        assert_eq!(ghost.prefix, "\\fr");
        assert_eq!(ghost.suffix, "ac");
        assert_eq!(ghost.full, "\\frac");
    }

    #[test]
    fn test_ruby_suggestion() {
        let nodes = to_nodes("逆鱗");
        let ghost = find_suggestion(0, 2, &nodes, "逆鱗", None, None, None)
            .expect("Should suggest gekirin for 逆鱗");
        assert_eq!(ghost.prefix, "逆鱗");
        let test_words = [
            ("相殺", 2, "そうさい"),
            ("独擅場", 3, "どくせんじょう"),
            ("稀薄", 2, "きはく"),
            ("希薄", 2, "きはく"),
            ("代替", 2, "だいたい"),
            ("出納", 2, "すいのう"),
        ];
        for (word, len, reading) in test_words {
            let nodes = to_nodes(word);
            let ghost = find_suggestion(0, len, &nodes, word, None, None, None)
                .unwrap_or_else(|| panic!("Should suggest ruby for {word}"));
            assert_eq!(ghost.ruby, Some((len, reading.into())));
        }

        let mut custom_rubies = std::collections::HashMap::new();
        custom_rubies.insert("独自単語".into(), "どくじ".into());
        let nodes2 = to_nodes("独自単語");
        let ghost2 = find_suggestion(0, 4, &nodes2, "独自単語", None, None, Some(&custom_rubies))
            .expect("Should suggest custom ruby");
        assert_eq!(ghost2.prefix, "独自単語");
        assert_eq!(ghost2.ruby, Some((4, "どくじ".into())));
    }
}
