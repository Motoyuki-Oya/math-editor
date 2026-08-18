//! 検索パターンの選択と一致処理を提供します。

use regex::Regex;

use super::boyer_moore::BoyerMoore;
use super::SearchOptions;

/// コンパイル済みの検索パターンです。
#[derive(Debug)]
pub(super) enum Matcher {
    Regex(Regex),
    Literal(BoyerMoore),
}

impl Matcher {
    /// 1 つの文字列内の一致を返します。
    pub(super) fn matches(&self, run: &str) -> Vec<(usize, usize, Vec<String>)> {
        match self {
            Self::Regex(regex) => regex_matches(regex, run),
            Self::Literal(boyer_moore) => {
                // 一致ごとに `run` を数え直すと一致の数だけ走査が増えるため、文字は一度だけ集めます。
                let chars: Vec<char> = run.chars().collect();
                boyer_moore
                    .find(run)
                    .into_iter()
                    .map(|(from, to)| (from, to, vec![chars[from..to].iter().collect()]))
                    .collect()
            }
        }
    }
}

/// `regex` クレートを使って一致を探します。
fn regex_matches(regex: &Regex, run: &str) -> Vec<(usize, usize, Vec<String>)> {
    let mut found = Vec::new();
    let mut byte_starts = Vec::new();
    let mut end = 0;
    for (byte, c) in run.char_indices() {
        byte_starts.push(byte);
        end = byte + c.len_utf8();
    }
    byte_starts.push(end);
    let byte_to_char = |byte: usize| byte_starts.binary_search(&byte).unwrap_or_else(|pos| pos);

    if regex.captures_len() == 1 {
        for m in regex.find_iter(run) {
            let from = byte_to_char(m.start());
            let to = byte_to_char(m.end());
            found.push((from, to, vec![m.as_str().to_string()]));
        }
    } else {
        for caps in regex.captures_iter(run) {
            let m = caps.get(0).expect("group 0 is always present");
            let from = byte_to_char(m.start());
            let to = byte_to_char(m.end());
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

/// リテラル検索で `regex` クレートに切り替えるファイルサイズの仮の閾値（バイト数）。
const LITERAL_REGEX_THRESHOLD: usize = 100_000;

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
    file_size: Option<usize>,
) -> Option<Matcher> {
    if query.is_empty() {
        return None;
    }
    let large_file = file_size.is_none_or(|size| size > LITERAL_REGEX_THRESHOLD);
    let use_regex = options.regex || large_file;
    if use_regex {
        let pattern = if options.regex {
            query.to_string()
        } else {
            regex::escape(query)
        };
        build_regex(&pattern, options.case_sensitive).map(Matcher::Regex)
    } else {
        BoyerMoore::new(query, options.case_sensitive).map(Matcher::Literal)
    }
}

#[cfg(test)]
/// テスト用: JavaScriptCore の RegExp が返す UTF-16 単位を文字インデックスに変換します。
fn char_starts(text: &str) -> Vec<usize> {
    let mut units = Vec::with_capacity(text.chars().count() + 1);
    let mut at = 0;
    for c in text.chars() {
        units.push(at);
        at += c.len_utf16();
    }
    units.push(at);
    units
}

#[cfg(test)]
fn char_of(starts: &[usize], unit: usize) -> Option<usize> {
    starts.iter().position(|start| *start == unit)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SMALL_FILE: Option<usize> = Some(0);

    #[test]
    fn an_unknown_file_size_falls_back_to_the_regex_crate() {
        let options = SearchOptions::default();
        assert!(matches!(
            compile("abc", options, None),
            Some(Matcher::Regex(_))
        ));
        assert!(matches!(
            compile("abc", options, SMALL_FILE),
            Some(Matcher::Literal(_))
        ));
        assert!(matches!(
            compile("abc", options, Some(LITERAL_REGEX_THRESHOLD + 1)),
            Some(Matcher::Regex(_))
        ));
    }

    fn compile_regex_crate(query: &str, options: SearchOptions) -> Option<regex::Regex> {
        if query.is_empty() {
            return None;
        }
        let pattern = if options.regex {
            query.to_string()
        } else {
            regex::escape(query)
        };
        build_regex(&pattern, options.case_sensitive)
    }

    #[test]
    fn character_columns_come_from_utf16_offsets() {
        let starts = char_starts("あa𝑥b");
        assert_eq!(starts, vec![0, 1, 2, 4, 5]);
        assert_eq!(char_of(&starts, 2), Some(2));
    }

    #[test]
    fn boyer_moore_vs_regex_crate() {
        use std::time::Instant;

        let cases = [
            ("abc".repeat(10_000) + "findme", "findme"),
            ("findme".to_string() + &"abc".repeat(10_000), "findme"),
            ("abc".repeat(10_000), "xyz"),
            ("ab".repeat(10_000), "ab"),
        ];
        let options = SearchOptions {
            regex: false,
            case_sensitive: true,
        };
        let rounds = 100;

        for (text, query) in cases {
            let Some(matcher) = compile(query, options, SMALL_FILE) else {
                panic!("literal pattern should compile");
            };
            let Some(re) = compile_regex_crate(query, options) else {
                panic!("regex crate pattern should compile");
            };

            let t0 = Instant::now();
            let mut bm = Vec::new();
            for _ in 0..rounds {
                bm = matcher.matches(&text);
            }
            let dt_bm = t0.elapsed();

            let t1 = Instant::now();
            let mut re_matches = Vec::new();
            for _ in 0..rounds {
                re_matches = regex_matches(&re, &text);
            }
            let dt_re = t1.elapsed();

            assert_eq!(bm, re_matches);
            println!(
                "query={query:?} text_len={} Boyer-Moore: {dt_bm:?}, regex crate: {dt_re:?}",
                text.len()
            );
        }
    }

    #[cfg(target_os = "linux")]
    mod webkitgtk_regexp {
        use super::*;
        use javascriptcore::{Context, ContextExt, Value, ValueExt};

        #[derive(Debug, serde::Deserialize)]
        struct JscMatch {
            index: usize,
            groups: Vec<String>,
        }

        fn jsc_regexp_matches(
            ctx: &Context,
            pattern: &str,
            text: &str,
            case_insensitive: bool,
        ) -> Option<Vec<(usize, usize, Vec<String>)>> {
            ctx.set_value("P", &Value::new_string(ctx, Some(pattern)));
            let flags = if case_insensitive { "gi" } else { "g" };
            ctx.set_value("F", &Value::new_string(ctx, Some(flags)));
            ctx.set_value("T", &Value::new_string(ctx, Some(text)));

            let script = r#"
                var re = new RegExp(P, F);
                var text = T;
                var out = [];
                var match;
                var limit = 0;
                while ((match = re.exec(text)) !== null) {
                    var groups = [];
                    for (var i = 0; i < match.length; i++) {
                        groups.push(match[i] === undefined ? "" : match[i]);
                    }
                    out.push({ index: match.index, groups: groups });
                    if (match[0].length === 0) {
                        var lead = text.charCodeAt(match.index);
                        if ((lead & 0xFC00) === 0xD800 && match.index + 1 < text.length) {
                            re.lastIndex = match.index + 2;
                        } else {
                            re.lastIndex = match.index + 1;
                        }
                    }
                    if (++limit > 100000) break;
                }
                JSON.stringify(out);
            "#;

            let value = ctx.evaluate(script).or_else(|| {
                let msg = ctx
                    .exception()
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "JSC evaluation failed".to_string());
                eprintln!("JSC evaluate error: {msg}");
                None
            })?;
            let json = value.to_str().to_string();
            let matches: Vec<JscMatch> = serde_json::from_str(&json).ok()?;
            let units = char_starts(text);
            let mut found = Vec::new();
            for m in matches {
                let whole = m.groups.first().cloned().unwrap_or_default();
                let end = m.index + whole.encode_utf16().count();
                let from = char_of(&units, m.index)?;
                let to = char_of(&units, end)?;
                found.push((from, to, m.groups));
            }
            Some(found)
        }

        #[test]
        fn webkitgtk_regexp_vs_regex_crate() {
            use std::time::Instant;

            let ctx = Context::new();
            let rounds = 10;
            let cases: [(&str, &str, SearchOptions); 6] = [
                (
                    "findme",
                    &("abc".repeat(2_000) + "findme"),
                    SearchOptions {
                        regex: false,
                        case_sensitive: true,
                    },
                ),
                (
                    "findme",
                    &("findme".to_string() + &"abc".repeat(2_000)),
                    SearchOptions {
                        regex: false,
                        case_sensitive: true,
                    },
                ),
                (
                    "xyz",
                    &"abc".repeat(2_000),
                    SearchOptions {
                        regex: false,
                        case_sensitive: true,
                    },
                ),
                (
                    "ab",
                    &"ab".repeat(1_000),
                    SearchOptions {
                        regex: false,
                        case_sensitive: true,
                    },
                ),
                // 正規表現モードでも比較
                (
                    "a.c",
                    &"abc".repeat(1_000),
                    SearchOptions {
                        regex: true,
                        case_sensitive: true,
                    },
                ),
                (
                    "a+",
                    &"a".repeat(5_000),
                    SearchOptions {
                        regex: true,
                        case_sensitive: true,
                    },
                ),
            ];

            for (query, text, options) in cases {
                let pattern = if options.regex {
                    query.to_string()
                } else {
                    regex::escape(query)
                };

                let Some(re) = compile_regex_crate(query, options) else {
                    panic!("regex crate should compile for {query:?}");
                };

                let t0 = Instant::now();
                let mut js_results = Vec::new();
                for _ in 0..rounds {
                    js_results = jsc_regexp_matches(&ctx, &pattern, text, !options.case_sensitive)
                        .expect("JSC RegExp should run");
                }
                let dt_js = t0.elapsed();

                let t1 = Instant::now();
                let mut re_results = Vec::new();
                for _ in 0..rounds {
                    re_results = regex_matches(&re, text);
                }
                let dt_re = t1.elapsed();

                assert_eq!(
                    js_results, re_results,
                    "JSC RegExp and regex crate differ for {query:?}"
                );
                println!(
                    "query={query:?} regex={} text_len={} \
                     JSC RegExp: {dt_js:?}, regex crate: {dt_re:?}",
                    options.regex,
                    text.len()
                );
            }
        }
    }
}
