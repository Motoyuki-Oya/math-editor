//! 検索パターンの選択と一致処理を提供します。

use regex::Regex;
use unicode_normalization::UnicodeNormalization;

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

/// `memchr::memmem` と `unicode-normalization` を使ったクエリ展開方式の高速リテラルマッチャー。
#[derive(Debug)]
pub(super) struct LiteralMatcher {
    variants: Vec<PatternVariant>,
    regex_ci: Option<Regex>,
}

#[derive(Debug)]
struct PatternVariant {
    bytes: Box<[u8]>,
    char_count: usize,
    byte_len: usize,
}

impl LiteralMatcher {
    pub(super) fn new(query: &str, case_sensitive: bool) -> Option<Self> {
        if query.is_empty() {
            return None;
        }

        // クエリから NFC版 と NFD版 を生成
        let nfc: String = query.nfc().collect();
        let nfd: String = query.nfd().collect();

        let mut variant_strings = vec![nfc];
        if !variant_strings.contains(&nfd) {
            variant_strings.push(nfd);
        }

        let mut variants = Vec::with_capacity(variant_strings.len());
        if case_sensitive {
            for s in &variant_strings {
                let char_count = s.chars().count();
                let byte_len = s.len();
                let bytes = s.as_bytes().to_vec().into_boxed_slice();
                variants.push(PatternVariant {
                    bytes,
                    char_count,
                    byte_len,
                });
            }
        }

        let regex_ci = if !case_sensitive {
            let escaped: Vec<String> = variant_strings.iter().map(|s| regex::escape(s)).collect();
            let pattern = if escaped.len() == 1 {
                escaped[0].clone()
            } else {
                format!("(?:{})", escaped.join("|"))
            };
            regex::RegexBuilder::new(&pattern)
                .case_insensitive(true)
                .build()
                .ok()
        } else {
            None
        };

        Some(Self { variants, regex_ci })
    }

    pub(super) fn matches(&self, run: &str) -> Vec<(usize, usize, Vec<String>)> {
        if run.is_empty() {
            return Vec::new();
        }

        if let Some(re) = &self.regex_ci {
            return regex_matches(re, run);
        }

        // 単一パターンの場合（合成文字なし: 最頻パス）
        if self.variants.len() == 1 {
            let v = &self.variants[0];
            let mut found = Vec::new();
            for byte_pos in memchr::memmem::find_iter(run.as_bytes(), &v.bytes) {
                let from = run[..byte_pos].chars().count();
                let to = from + v.char_count;
                let matched_text = run[byte_pos..byte_pos + v.byte_len].to_string();
                found.push((from, to, vec![matched_text]));
            }
            return found;
        }

        // 複数パターンの場合（NFC / NFD 展開）
        let mut matches_raw: Vec<(usize, usize, usize)> = Vec::new(); // (byte_pos, byte_len, char_count)
        for v in &self.variants {
            for byte_pos in memchr::memmem::find_iter(run.as_bytes(), &v.bytes) {
                matches_raw.push((byte_pos, v.byte_len, v.char_count));
            }
        }

        if matches_raw.is_empty() {
            return Vec::new();
        }

        // 出現バイト位置順にソートし重複を除去
        matches_raw.sort_by_key(|(pos, _, _)| *pos);
        matches_raw.dedup_by_key(|(pos, _, _)| *pos);

        let mut found = Vec::with_capacity(matches_raw.len());
        for (byte_pos, byte_len, char_count) in matches_raw {
            let from = run[..byte_pos].chars().count();
            let to = from + char_count;
            let matched_text = run[byte_pos..byte_pos + byte_len].to_string();
            found.push((from, to, vec![matched_text]));
        }

        found
    }
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

    #[test]
    fn benchmark_text_normalization_vs_query_expansion() {
        use std::time::Instant;

        // 100,000行のデータセット（NFD分解文字列・NFC合成文字列が混在）
        let mut lines = Vec::with_capacity(100_000);
        for i in 0..100_000 {
            if i % 1000 == 0 {
                // NFD 結合文字行
                lines.push(format!(
                    "pub fn か\u{3099}関数_{i}() -> Result<cafe\u{0301}, Error> {{ }}"
                ));
            } else if i % 100 == 0 {
                // NFC 合成文字行
                lines.push(format!(
                    "// 日本語コメント: が関数 を呼び出しています（行: {i}）"
                ));
            } else {
                lines.push(format!(
                    "let x_{i} = i * 42 + compute_something_else(12345);"
                ));
            }
        }

        let query = "が関数";

        // ==========================================
        // 方式1: テキスト側正規化方式（前回の実装を再現）
        use unicode_normalization::{is_nfc_quick, IsNormalized};

        fn is_comb(c: char) -> bool {
            matches!(c, '\u{0300}'..='\u{036F}' | '\u{3099}' | '\u{309A}')
        }
        fn norm_with_ranges(run: &str) -> (String, Vec<(usize, usize)>) {
            let mut norm_text = String::new();
            let mut ranges = Vec::new();
            let orig: Vec<char> = run.chars().collect();
            let n = orig.len();
            let mut i = 0;
            while i < n {
                let start = i;
                i += 1;
                while i < n && is_comb(orig[i]) {
                    i += 1;
                }
                let cluster: String = orig[start..i].iter().collect();
                for nc in cluster.nfc() {
                    norm_text.push(nc);
                    ranges.push((start, i));
                }
            }
            (norm_text, ranges)
        }

        let query_nfc: String = query.nfc().collect();
        let query_char_count = query_nfc.chars().count();
        let finder = memchr::memmem::Finder::new(query_nfc.as_bytes());

        let t0 = Instant::now();
        let mut count1 = 0;
        for line in &lines {
            if is_nfc_quick(line.chars()) == IsNormalized::Yes {
                for byte_pos in finder.find_iter(line.as_bytes()) {
                    let from = line[..byte_pos].chars().count();
                    let to = from + query_char_count;
                    let _ = (from, to);
                    count1 += 1;
                }
            } else {
                let (norm_text, ranges) = norm_with_ranges(line);
                for byte_pos in finder.find_iter(norm_text.as_bytes()) {
                    let norm_from = norm_text[..byte_pos].chars().count();
                    let norm_to = norm_from + query_char_count;
                    if norm_from < ranges.len() && norm_to <= ranges.len() {
                        let from = ranges[norm_from].0;
                        let to = ranges[norm_to - 1].1;
                        let _ = (from, to);
                        count1 += 1;
                    }
                }
            }
        }
        let dt_text_norm = t0.elapsed();

        // ==========================================
        // 方式2: クエリ側展開方式（今回の実装）
        // ==========================================
        let matcher = LiteralMatcher::new(query, true).unwrap();

        let t1 = Instant::now();
        let mut count2 = 0;
        for line in &lines {
            let m = matcher.matches(line);
            count2 += m.len();
        }
        let dt_query_exp = t1.elapsed();

        println!("\n=== Benchmark: Text Normalization vs Query Expansion (100,000 lines) ===");
        println!(
            "1. Text Normalization: {:?} (matches={count1})",
            dt_text_norm
        );
        println!(
            "2. Query Expansion:    {:?} (matches={count2})",
            dt_query_exp
        );
        assert_eq!(count1, count2);

        // 連続バッファ直接走査（100,000行）
        let full_text: String = lines.join("\n");
        let bytes = full_text.as_bytes();
        let t2 = Instant::now();
        let mut buffer_count = 0;
        for v in &matcher.variants {
            for byte_pos in memchr::memmem::find_iter(bytes, &v.bytes) {
                buffer_count += 1;
                let _ = byte_pos;
            }
        }
        let dt_buffer = t2.elapsed();
        let gb_per_sec = (bytes.len() as f64 / 1_000_000_000.0) / dt_buffer.as_secs_f64();
        println!(
            "3. Query Expansion on Whole Buffer ({:.2} MB):\n   Time: {:?}\n   Throughput: {:.2} GB/s (matches={buffer_count})",
            bytes.len() as f64 / (1024.0 * 1024.0),
            dt_buffer,
            gb_per_sec
        );
    }
}
