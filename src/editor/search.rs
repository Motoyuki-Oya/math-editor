//! オプションの正規表現を使用して、モデルを検索して置換します。
//!
//! 通常の（正規表現でない）検索は Rust 側で Boyer-Moore 法を使います。
//! 長大な文書や正規表現の検索には `regex` クレートを使用します。
//! 不適切なパターンはコンパイルに失敗するだけで、エディターが中断されることはありません。
//! 構造も同様に行ごとに検索されます。一致とは、文書内のどこにあっても、
//! 互いに隣り合った一連の文字です。行の端にまたがることがないのと同じように、
//! 一致は構造の端にまたがることはありません。

use std::collections::HashMap;

use regex::Regex;

use crate::structure::ast::{Cursor, Node, Row};
use crate::structure::text::{Item, Pos, Sel, Text};

/// 一致の場所。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    /// 通常のテキストのストレッチ。
    Text(Sel),
    /// 構造内の `at` にある 1 行のストレッチ。
    Inside { at: Pos, cursor: Cursor },
}

/// 一致: 一致の場所と、キャプチャされたグループ。
#[derive(Clone, Debug)]
pub struct Found {
    pub place: Place,
    pub groups: Vec<String>,
}

/// 読み取り順序の場所。これは、「次を検索」が最後の一致から引き継ぐ方法です。テキスト内のアイテム、その後
pub type Key = (Pos, Option<(Vec<(usize, usize)>, usize)>);

impl Place {
    pub fn start(&self) -> Key {
        match self {
            Place::Text(sel) => (sel.start(), None),
            Place::Inside { at, cursor } => (*at, Some((cursor.path.clone(), cursor.start()))),
        }
    }

    pub fn end(&self) -> Key {
        match self {
            Place::Text(sel) => (sel.end(), None),
            Place::Inside { at, cursor } => (*at, Some((cursor.path.clone(), cursor.end()))),
        }
    }
}

/// まだ何も見つからないときに検索が開始される場所: テキスト内であっても構造内であっても、キャレットです。
pub fn key_at(at: Pos, inside: Option<&Cursor>) -> Key {
    (at, inside.map(|cursor| (cursor.path.clone(), cursor.end())))
}

#[derive(Clone, Copy, Default)]
pub struct SearchOptions {
    pub regex: bool,
    pub case_sensitive: bool,
}

/// コンパイル済みの検索パターン。正規表現または長大なファイルは `regex` クレート、
/// それ以外は Boyer-Moore 法で検索します。
#[derive(Debug)]
enum Matcher {
    Regex(Regex),
    Literal {
        pattern: Vec<char>,
        case_sensitive: bool,
        bad_char: HashMap<char, usize>,
        good_suffix: Vec<usize>,
    },
}

pub fn find_next(
    text: &Text,
    query: &str,
    options: SearchOptions,
    file_size: Option<usize>,
    from: Key,
) -> Option<Found> {
    let all = find_all(text, query, options, file_size);
    all.iter()
        .find(|found| found.place.start() >= from)
        .or_else(|| all.first())
        .cloned()
}

pub fn find_all(
    text: &Text,
    query: &str,
    options: SearchOptions,
    file_size: Option<usize>,
) -> Vec<Found> {
    let Some(matcher) = compile(query, options, file_size) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for line in 0..text.line_count() {
        let items = text.line(line);
        for (start, run) in runs(items) {
            for (from, to, groups) in matches(&matcher, &run) {
                found.push(Found {
                    place: Place::Text(Sel::range(
                        Pos::new(line, start + from),
                        Pos::new(line, start + to),
                    )),
                    groups,
                });
            }
        }
        // 構造内の行も検索されるため、分数に埋もれた名前も他の行と同様に検索されます。
        for (col, item) in items.iter().enumerate() {
            let Item::Math(root) = item else { continue };
            let at = Pos::new(line, col);
            search_row(&matcher, root, &mut Vec::new(), &mut |cursor, groups| {
                found.push(Found {
                    place: Place::Inside { at, cursor },
                    groups,
                })
            });
        }
    }
    found.sort_by_key(|found| found.place.start());
    found
}

/// 1 つの行とその中にネストされているすべての行を検索し、各一致をその行の選択として報告します。
fn search_row(
    matcher: &Matcher,
    row: &Row,
    path: &mut Vec<(usize, usize)>,
    report: &mut impl FnMut(Cursor, Vec<String>),
) {
    for (start, run) in node_runs(row) {
        for (from, to, groups) in matches(matcher, &run) {
            report(
                Cursor {
                    path: path.clone(),
                    anchor: start + from,
                    index: start + to,
                    fills: Vec::new(),
                },
                groups,
            );
        }
    }
    for (index, node) in row.iter().enumerate() {
        for slot in 0..node.slot_count() {
            let Some(nested) = node.slot(slot) else {
                continue;
            };
            path.push((index, slot));
            search_row(matcher, nested, path, report);
            path.pop();
        }
    }
}

fn matches(matcher: &Matcher, run: &str) -> Vec<(usize, usize, Vec<String>)> {
    match matcher {
        Matcher::Regex(regex) => regex_matches(regex, run),
        Matcher::Literal { .. } => literal_matches(matcher, run),
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

/// Boyer-Moore 法でリテラル一致を探します。
fn literal_matches(matcher: &Matcher, run: &str) -> Vec<(usize, usize, Vec<String>)> {
    let Matcher::Literal {
        pattern,
        case_sensitive,
        bad_char,
        good_suffix,
    } = matcher
    else {
        unreachable!("literal_matches called with a non-literal matcher")
    };
    let m = pattern.len();
    if m == 0 {
        return Vec::new();
    }

    let run_chars: Vec<char> = run.chars().collect();
    let (text, mapping) = if *case_sensitive {
        (run_chars.clone(), None)
    } else {
        let mut text = Vec::with_capacity(run_chars.len());
        let mut map = Vec::with_capacity(run_chars.len());
        for (i, c) in run_chars.iter().enumerate() {
            for lc in c.to_lowercase() {
                text.push(lc);
                map.push(i);
            }
        }
        (text, Some(map))
    };

    let n = text.len();
    let mut found = Vec::new();
    let mut s = 0;
    while s + m <= n {
        let mut j: isize = m as isize - 1;
        while j >= 0 && text[(s as isize + j) as usize] == pattern[j as usize] {
            j -= 1;
        }
        if j < 0 {
            let (from, to) = if let Some(map) = &mapping {
                let from = map[s];
                let to = map[s + m - 1] + 1;
                (from, to)
            } else {
                (s, s + m)
            };
            let matched = run_chars[from..to].iter().collect();
            found.push((from, to, vec![matched]));
            s += good_suffix[0];
        } else {
            let ju = j as usize;
            let c = text[s + ju];
            let k = bad_char.get(&c).copied().map(|k| k as isize).unwrap_or(-1);
            let bc = ((ju as isize - k).max(1)) as usize;
            s += bc.max(good_suffix[ju]);
        }
    }
    found
}

/// 置換内の `$1` スタイルの参照をキャプチャされたもので埋め、列の区切り文字および改行として `\t` と `\n` を読み取ります。
///
/// 正規表現ボックスを使用しない場合、置換はクエリと同じように文字通りに解釈されます。
pub fn expand(groups: &[String], replacement: &str, options: SearchOptions) -> String {
    if !options.regex {
        return replacement.to_string();
    }
    let mut out = String::new();
    let mut chars = replacement.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('t') => out.push('\t'),
                Some('n') => out.push('\n'),
                Some('\\') => out.push('\\'),
                // 他のものの前にあるバックスラッシュはそれ自体を表すため、Windows パスはすべてを 2 倍にすることなく書き込むことができます。 one.
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
            continue;
        }
        if c != '$' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('$') => {
                chars.next();
                out.push('$');
            }
            Some('&') => {
                chars.next();
                out.push_str(groups.first().map(String::as_str).unwrap_or_default());
            }
            Some(digit) if digit.is_ascii_digit() => {
                let mut number = String::new();
                while let Some(digit) = chars.peek().filter(|c| c.is_ascii_digit()) {
                    number.push(*digit);
                    chars.next();
                }
                let index: usize = number.parse().unwrap_or(0);
                out.push_str(groups.get(index).map(String::as_str).unwrap_or_default());
            }
            _ => out.push('$'),
        }
    }
    out
}

/// テキストの項目としての置換: タブは列の区切り文字であり、このエディターでのタブのようなもので、改行で改行されます。
pub fn replacement_items(text: &str) -> Vec<Vec<Item>> {
    text.split('\n')
        .map(|line| {
            line.chars()
                .map(|c| if c == '\t' { Item::Tab } else { Item::Char(c) })
                .collect()
        })
        .collect()
}

/// 行内の通常の文字のストレッチ。それぞれの列が開始されます。数式はそれらを分割するため、一致は 1 つにまたがることはできません。
fn runs(items: &[Item]) -> Vec<(usize, String)> {
    let mut runs = Vec::new();
    let mut run = String::new();
    let mut start = 0;
    for (col, item) in items.iter().enumerate() {
        match item.as_char() {
            Some(c) => {
                if run.is_empty() {
                    start = col;
                }
                run.push(c);
            }
            None => {
                if !run.is_empty() {
                    runs.push((start, std::mem::take(&mut run)));
                }
            }
        }
    }
    if !run.is_empty() {
        runs.push((start, run));
    }
    runs
}

/// 構造の行の場合は「runs」と同じです。単純な文字のみが一致するため、独自の形状を持つものはすべて実行を中断します。
fn node_runs(row: &Row) -> Vec<(usize, String)> {
    let mut runs = Vec::new();
    let mut run = String::new();
    let mut start = 0;
    for (index, node) in row.iter().enumerate() {
        match node {
            Node::Char(c) => {
                if run.is_empty() {
                    start = index;
                }
                run.push(*c);
            }
            _ => {
                if !run.is_empty() {
                    runs.push((start, std::mem::take(&mut run)));
                }
            }
        }
    }
    if !run.is_empty() {
        runs.push((start, run));
    }
    runs
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

/// リテラル検索で `regex` クレートに切り替えるファイルサイズの仮の閾値（バイト数）。
const LITERAL_REGEX_THRESHOLD: usize = 100_000;

fn build_regex(pattern: &str, case_sensitive: bool) -> Option<Regex> {
    regex::RegexBuilder::new(pattern)
        .case_insensitive(!case_sensitive)
        .build()
        .ok()
}

/// コンパイル済みの検索パターンを作成します。正規表現または長大なファイルは `regex` クレート、
/// それ以外は Boyer-Moore 法を使います。`file_size` が `None`（ファイルも下書きも読めず
/// 大きさが分からない）ときは安全側に寄せて `regex` クレートを使います。
fn compile(query: &str, options: SearchOptions, file_size: Option<usize>) -> Option<Matcher> {
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
        let pattern: Vec<char> = if options.case_sensitive {
            query.chars().collect()
        } else {
            query.to_lowercase().chars().collect()
        };
        let m = pattern.len();
        if m == 0 {
            return None;
        }
        let mut bad_char = HashMap::new();
        for (i, c) in pattern.iter().enumerate() {
            bad_char.insert(*c, i);
        }
        let good_suffix = build_good_suffix(&pattern);
        Some(Matcher::Literal {
            pattern,
            case_sensitive: options.case_sensitive,
            bad_char,
            good_suffix,
        })
    }
}

/// Z-関数に基づく suffix テーブル。good-suffix テーブルの計算に使います。
fn suffixes(p: &[char]) -> Vec<usize> {
    let m = p.len();
    let mut suff = vec![0; m];
    suff[m - 1] = m;
    let mut g = (m - 1) as isize;
    let mut f = (m - 1) as isize;
    for i in (0..m - 1).rev() {
        let i_i = i as isize;
        let offset = i_i + m as isize - 1 - f;
        if i_i > g && offset >= 0 && suff[offset as usize] < (i_i - g) as usize {
            suff[i] = suff[offset as usize];
        } else {
            if i_i < g {
                g = i_i;
            }
            f = i_i;
            while g >= 0 && p[g as usize] == p[(g + m as isize - 1 - f) as usize] {
                g -= 1;
            }
            suff[i] = (f - g) as usize;
        }
    }
    suff
}

/// Boyer-Moore の good-suffix シフトテーブルを構築します。
fn build_good_suffix(p: &[char]) -> Vec<usize> {
    let m = p.len();
    let suff = suffixes(p);
    let mut gs = vec![m; m];
    let mut j = 0;
    for i in (0..m).rev() {
        if suff[i] == i + 1 {
            let target = m - 1 - i;
            while j < target {
                if gs[j] == m {
                    gs[j] = m - 1 - i;
                }
                j += 1;
            }
        }
    }
    for (i, &s) in suff.iter().enumerate().take(m - 1) {
        let index = m - 1 - s;
        if index < m {
            gs[index] = m - 1 - i;
        }
    }
    gs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_fill_the_replacement() {
        let groups = vec!["a@b".to_string(), "a".to_string(), "b".to_string()];
        let options = SearchOptions {
            regex: true,
            case_sensitive: false,
        };
        assert_eq!(expand(&groups, "$2:$1", options), "b:a");
        assert_eq!(expand(&groups, "[$&]", options), "[a@b]");
        assert_eq!(expand(&groups, "$$1", options), "$1");
        // 正規表現ボックスを使用しない場合、置換はリテラルです。
        assert_eq!(expand(&groups, "$1", SearchOptions::default()), "$1");
    }

    #[test]
    fn escapes_stand_for_a_tab_and_a_new_line() {
        let options = SearchOptions {
            regex: true,
            case_sensitive: false,
        };
        let groups = vec!["x".to_string(), "x".to_string()];
        assert_eq!(expand(&groups, "$1\\t=", options), "x\t=");
        assert_eq!(expand(&groups, "a\\nb", options), "a\nb");
        assert_eq!(expand(&groups, "a\\\\t", options), "a\\t");
        // リテラル置換はバックスラッシュを保持します。
        assert_eq!(expand(&groups, "a\\tb", SearchOptions::default()), "a\\tb");
    }

    /// 置換内のタブは列の区切り文字であり、Tab キーで挿入されます。'=' を '\t=` に置換すると、列が整列されます。
    #[test]
    fn a_replaced_tab_is_a_column_separator() {
        assert_eq!(
            replacement_items("x\t=\ny"),
            vec![
                vec![Item::Char('x'), Item::Tab, Item::Char('=')],
                vec![Item::Char('y')],
            ]
        );
    }

    #[test]
    fn islands_break_up_the_searched_text() {
        let text = Text::from_lines(vec![vec![
            Item::Char('a'),
            Item::Char('b'),
            Item::Math(vec![crate::structure::ast::Node::Char('x')]),
            Item::Char('c'),
            Item::Char('d'),
        ]]);
        let runs = runs(text.line(0));
        assert_eq!(runs, vec![(0, "ab".to_string()), (3, "cd".to_string())]);
    }

    /// 構造内で一致できるのはプレーン文字のみです。独自の形状は、数式が改行するのと同じように、行を分割します。
    #[test]
    fn a_row_of_a_structure_is_searched_as_its_characters() {
        let row = vec![
            Node::Char('a'),
            Node::Char('b'),
            crate::structure::ast::sqrt(),
            Node::Char('c'),
        ];
        assert_eq!(
            node_runs(&row),
            vec![(0, "ab".to_string()), (3, "c".to_string())]
        );
    }

    /// 読み取り順序: 構造の前のテキスト、次にその内部にあるもの、その後に深い行となるため、次の一致を見つけると、ドキュメントを 1 回探索します。
    #[test]
    fn matches_are_ordered_by_where_they_are() {
        let text = Place::Text(Sel::range(Pos::new(0, 0), Pos::new(0, 1)));
        let shallow = Place::Inside {
            at: Pos::new(0, 1),
            cursor: Cursor::root(0),
        };
        let deep = Place::Inside {
            at: Pos::new(0, 1),
            cursor: Cursor {
                path: vec![(0, 0)],
                anchor: 0,
                index: 1,
                fills: Vec::new(),
            },
        };
        let mut places = vec![deep.clone(), text.clone(), shallow.clone()];
        places.sort_by_key(|place| place.start());
        assert_eq!(places, vec![text, shallow, deep]);
    }

    #[test]
    fn character_columns_come_from_utf16_offsets() {
        let starts = char_starts("あa𝑥b");
        assert_eq!(starts, vec![0, 1, 2, 4, 5]);
        assert_eq!(char_of(&starts, 2), Some(2));
    }

    /// Boyer-Moore 法を使う十分に小さいファイルを表します。
    const SMALL_FILE: Option<usize> = Some(0);

    /// ファイルサイズが分からないときは `regex` クレートに寄せます。
    #[test]
    fn an_unknown_file_size_falls_back_to_the_regex_crate() {
        let options = SearchOptions::default();
        assert!(matches!(
            compile("abc", options, None),
            Some(Matcher::Regex(_))
        ));
        assert!(matches!(
            compile("abc", options, SMALL_FILE),
            Some(Matcher::Literal { .. })
        ));
        assert!(matches!(
            compile("abc", options, Some(LITERAL_REGEX_THRESHOLD + 1)),
            Some(Matcher::Regex(_))
        ));
    }

    /// リテラル検索は正規表現のメタ文字をただの文字として扱います。
    #[test]
    fn literal_queries_treat_metacharacters_as_text() {
        let text = Text::from_lines(vec![vec![
            Item::Char('a'),
            Item::Char('.'),
            Item::Char('b'),
            Item::Char('*'),
        ]]);
        let found = find_all(&text, "a.b*", SearchOptions::default(), SMALL_FILE);
        assert_eq!(found.len(), 1);
        if let Place::Text(sel) = &found[0].place {
            assert_eq!(sel.start(), Pos::new(0, 0));
            assert_eq!(sel.end(), Pos::new(0, 4));
        } else {
            panic!("expected text match");
        }
    }

    /// Boyer-Moore 法のリテラル検索は大文字小文字を区別しなくても動作します。
    #[test]
    fn literal_search_is_case_insensitive() {
        let text = Text::from_lines(vec![vec![
            Item::Char('A'),
            Item::Char('B'),
            Item::Char('a'),
            Item::Char('b'),
            Item::Char('A'),
        ]]);
        let found = find_all(
            &text,
            "ab",
            SearchOptions {
                regex: false,
                case_sensitive: false,
            },
            SMALL_FILE,
        );
        assert_eq!(found.len(), 2);
        if let (Place::Text(first), Place::Text(second)) = (&found[0].place, &found[1].place) {
            assert_eq!(first.start(), Pos::new(0, 0));
            assert_eq!(first.end(), Pos::new(0, 2));
            assert_eq!(second.start(), Pos::new(0, 2));
            assert_eq!(second.end(), Pos::new(0, 4));
        } else {
            panic!("expected two text matches");
        }
    }

    /// 同じ文字が連続するときでも、すべての重なり合う一致を見つけます。
    #[test]
    fn boyer_moore_finds_overlapping_matches() {
        let text = Text::from_lines(vec![vec![
            Item::Char('a'),
            Item::Char('a'),
            Item::Char('a'),
            Item::Char('a'),
        ]]);
        let found = find_all(&text, "aa", SearchOptions::default(), SMALL_FILE);
        assert_eq!(found.len(), 3);
    }

    /// 構造内の文字もリテラル検索で見つかります。
    #[test]
    fn literal_search_finds_characters_inside_structures() {
        let text = Text::from_lines(vec![vec![
            Item::Char('a'),
            Item::Math(vec![Node::Char('b'), Node::Char('c')]),
            Item::Char('d'),
        ]]);
        let found = find_all(&text, "bc", SearchOptions::default(), SMALL_FILE);
        assert_eq!(found.len(), 1);
        assert!(matches!(found[0].place, Place::Inside { .. }));
    }

    /// `regex` クレートを使って同じインターフェースで一致を探します（比較用）。
    /// リテラル検索は内部で正規表現化され、正規表現検索と統一できます。
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

    /// Boyer-Moore 法と `regex` クレートの結果と速度を比較します。
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
                bm = literal_matches(&matcher, &text);
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

    /// WebKitGTK の JavaScriptCore エンジン上で動作するブラウザー `RegExp` と
    /// `regex` クレートを同じ入力で比較します（テスト専用）。
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
