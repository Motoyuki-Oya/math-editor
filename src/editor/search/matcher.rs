//! 検索パターンの選択と一致処理を提供します。

use memchr::memmem::Finder;
use regex::Regex;
use unicode_normalization::{is_nfc_quick, IsNormalized, UnicodeNormalization};

use super::SearchOptions;

/// コンパイル済みの検索パターンです。
#[derive(Debug)]
pub(super) enum Matcher {
    Regex(Regex),
    Literal(Box<LiteralMatcher>),
}

impl Matcher {
    /// 1 つの文字列内の一致を返します。
    pub(super) fn matches(&self, run: &str) -> Vec<(usize, usize, Vec<String>)> {
        match self {
            Self::Regex(regex) => regex_matches(regex, run),
            Self::Literal(literal) => literal.matches(run),
        }
    }
}

/// `memchr::memmem` と `unicode-normalization` を使った高速リテラルマッチャー。
#[derive(Debug)]
pub(super) struct LiteralMatcher {
    query_char_count: usize,
    finder: Option<Finder<'static>>,
    regex_ci: Option<Regex>,
}

impl LiteralMatcher {
    pub(super) fn new(query: &str, case_sensitive: bool) -> Option<Self> {
        if query.is_empty() {
            return None;
        }
        let query_nfc: String = query.nfc().collect();
        let query_char_count = query_nfc.chars().count();
        if query_char_count == 0 {
            return None;
        }

        let finder = if case_sensitive {
            let leaked_bytes: &'static [u8] =
                Box::leak(query_nfc.as_bytes().to_vec().into_boxed_slice());
            Some(Finder::new(leaked_bytes))
        } else {
            None
        };

        let regex_ci = if !case_sensitive {
            let pattern = regex::escape(&query_nfc);
            regex::RegexBuilder::new(&pattern)
                .case_insensitive(true)
                .build()
                .ok()
        } else {
            None
        };

        Some(Self {
            query_char_count,
            finder,
            regex_ci,
        })
    }

    pub(super) fn matches(&self, run: &str) -> Vec<(usize, usize, Vec<String>)> {
        if run.is_empty() {
            return Vec::new();
        }

        // Fast-path: テキストが既に NFC かつ結合文字を含まない場合（ゼロアロケーション走査）
        if is_nfc_quick(run.chars()) == IsNormalized::Yes {
            if let Some(finder) = &self.finder {
                let mut found = Vec::new();
                for byte_pos in finder.find_iter(run.as_bytes()) {
                    let from = run[..byte_pos].chars().count();
                    let to = from + self.query_char_count;
                    let matched_text = run[byte_pos..byte_pos + finder.needle().len()].to_string();
                    found.push((from, to, vec![matched_text]));
                }
                return found;
            } else if let Some(re) = &self.regex_ci {
                return regex_matches(re, run);
            }
        }

        // Slow-path: NFD 分解文字や結合文字を含む場合の正規化マッピング検索
        self.matches_normalized(run)
    }

    fn matches_normalized(&self, run: &str) -> Vec<(usize, usize, Vec<String>)> {
        let (norm_text, norm_to_orig) = normalize_nfc_with_ranges(run);
        let mut found = Vec::new();

        if let Some(finder) = &self.finder {
            for byte_pos in finder.find_iter(norm_text.as_bytes()) {
                let norm_from = norm_text[..byte_pos].chars().count();
                let norm_to = norm_from + self.query_char_count;
                if norm_from < norm_to_orig.len() && norm_to <= norm_to_orig.len() {
                    let orig_from = norm_to_orig[norm_from].0;
                    let orig_to = norm_to_orig[norm_to - 1].1;
                    let matched_text: String = run
                        .chars()
                        .skip(orig_from)
                        .take(orig_to - orig_from)
                        .collect();
                    found.push((orig_from, orig_to, vec![matched_text]));
                }
            }
        } else if let Some(re) = &self.regex_ci {
            for m in re.find_iter(&norm_text) {
                let norm_from = norm_text[..m.start()].chars().count();
                let norm_to = norm_from + m.as_str().chars().count();
                if norm_from < norm_to_orig.len() && norm_to <= norm_to_orig.len() {
                    let orig_from = norm_to_orig[norm_from].0;
                    let orig_to = norm_to_orig[norm_to - 1].1;
                    let matched_text: String = run
                        .chars()
                        .skip(orig_from)
                        .take(orig_to - orig_from)
                        .collect();
                    found.push((orig_from, orig_to, vec![matched_text]));
                }
            }
        }

        found
    }
}

/// 結合文字（Combining mark）かどうかを判定します。
fn is_combining(c: char) -> bool {
    matches!(
        c,
        '\u{0300}'..='\u{036F}'
            | '\u{1AB0}'..='\u{1AFF}'
            | '\u{1DC0}'..='\u{1DFF}'
            | '\u{20D0}'..='\u{20FF}'
            | '\u{FE20}'..='\u{FE2F}'
            | '\u{3099}'
            | '\u{309A}'
    )
}

/// 文字列を文字クラスタ単位で NFC 正規化し、正規化後文字から元文字インデックス範囲へのマッピングを返します。
fn normalize_nfc_with_ranges(run: &str) -> (String, Vec<(usize, usize)>) {
    let mut norm_text = String::new();
    let mut ranges = Vec::new();

    let orig_chars: Vec<char> = run.chars().collect();
    let n = orig_chars.len();
    let mut i = 0;

    while i < n {
        let start = i;
        i += 1;
        // 後続の結合文字をまとめて1つのクラスタとする
        while i < n && is_combining(orig_chars[i]) {
            i += 1;
        }
        let cluster: String = orig_chars[start..i].iter().collect();
        for norm_char in cluster.nfc() {
            norm_text.push(norm_char);
            ranges.push((start, i));
        }
    }

    (norm_text, ranges)
}

/// `regex` クレートを使って一致を探します。
fn regex_matches(regex: &Regex, run: &str) -> Vec<(usize, usize, Vec<String>)> {
    let mut found = Vec::new();

    if regex.captures_len() == 1 {
        for m in regex.find_iter(run) {
            let from = run[..m.start()].chars().count();
            let to = from + m.as_str().chars().count();
            found.push((from, to, vec![m.as_str().to_string()]));
        }
    } else {
        for caps in regex.captures_iter(run) {
            let m = caps.get(0).expect("group 0 is always present");
            let from = run[..m.start()].chars().count();
            let to = from + m.as_str().chars().count();
            let groups = (0..caps.len())
                .map(|i| {
                    caps.get(i)
                        .map(|m| m.as_str().to_string())
                        .unwrap_or_default()
                })
                .collect();
            found.push((from, to, groups));
        }
    }
    found
}

fn build_regex(pattern: &str, case_sensitive: bool) -> Option<Regex> {
    regex::RegexBuilder::new(pattern)
        .case_insensitive(!case_sensitive)
        .build()
        .ok()
}

/// コンパイル済みの検索パターンを作成します。
pub(super) fn compile(
    query: &str,
    options: SearchOptions,
    _file_size: Option<usize>,
) -> Option<Matcher> {
    if query.is_empty() {
        return None;
    }
    if options.regex {
        build_regex(query, options.case_sensitive).map(Matcher::Regex)
    } else {
        LiteralMatcher::new(query, options.case_sensitive)
            .map(Box::new)
            .map(Matcher::Literal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_creates_appropriate_matcher() {
        let options = SearchOptions::default();
        // リテラル検索
        assert!(matches!(
            compile("abc", options, None),
            Some(Matcher::Literal(_))
        ));
        // 正規表現検索
        let re_options = SearchOptions {
            regex: true,
            case_sensitive: false,
        };
        assert!(matches!(
            compile("abc.*", re_options, None),
            Some(Matcher::Regex(_))
        ));
    }

    #[test]
    fn regex_matches_correctly_computes_utf8_char_offsets_and_groups() {
        let regex = regex::Regex::new(r"fn\s+([a-zA-Z_0-9]+)").unwrap();
        let text = "こんにちは pub fn foo_123() { // テスト\n fn bar()";
        let matches = regex_matches(&regex, text);
        assert_eq!(matches.len(), 2);
        // 1つ目: "fn foo_123" -> 先頭 "こんにちは pub " は 10 文字
        assert_eq!(matches[0].0, 10);
        assert_eq!(matches[0].1, 20);
        assert_eq!(
            matches[0].2,
            vec!["fn foo_123".to_string(), "foo_123".to_string()]
        );
        // 2つ目: "fn bar"
        assert_eq!(matches[1].0, 33);
        assert_eq!(matches[1].1, 39);
        assert_eq!(matches[1].2, vec!["fn bar".to_string(), "bar".to_string()]);
    }

    #[test]
    fn literal_matcher_handles_unicode_combining_characters() {
        // 1. クエリが NFC「が」(\u{304C})、テキストが NFD「か\u{3099}」
        let matcher = LiteralMatcher::new("が", true).unwrap();
        let text = "テスト か\u{3099}んじ"; // "テスト "=4, "か\u{3099}"=2文字(index 4..6), "んじ"=2
        let matches = matcher.matches(text);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].0, 4);
        assert_eq!(matches[0].1, 6);
        assert_eq!(matches[0].2, vec!["か\u{3099}".to_string()]);

        // 2. クエリが NFD「か\u{3099}」、テキストが NFC「が」
        let matcher = LiteralMatcher::new("か\u{3099}", true).unwrap();
        let text = "テスト がんじ"; // "テスト "=4, "が"=1文字(index 4..5)
        let matches = matcher.matches(text);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].0, 4);
        assert_eq!(matches[0].1, 5);
        assert_eq!(matches[0].2, vec!["が".to_string()]);

        // 3. ラテン文字アクセント: クエリ「café」(\u{00E9})、テキスト「cafe\u{0301}」
        let matcher = LiteralMatcher::new("café", true).unwrap();
        let text = "Welcome to cafe\u{0301}!";
        let matches = matcher.matches(text);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].0, 11);
        assert_eq!(matches[0].1, 16); // "cafe\u{0301}" は 5 文字 (c,a,f,e,\u{0301})
        assert_eq!(matches[0].2, vec!["cafe\u{0301}".to_string()]);

        // 4. 大文字小文字無視と合成文字の組み合わせ
        let matcher = LiteralMatcher::new("CAFÉ", false).unwrap();
        let text = "Welcome to cafe\u{0301}!";
        let matches = matcher.matches(text);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].0, 11);
        assert_eq!(matches[0].1, 16);
        assert_eq!(matches[0].2, vec!["cafe\u{0301}".to_string()]);
    }
}
