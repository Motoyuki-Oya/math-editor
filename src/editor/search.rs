//! オプションの正規表現を使用して、モデルを検索して置換します。
//!
//! 通常の（正規表現でない）検索は Rust 側で Boyer-Moore 法を使います。
//! 正規表現の検索だけがブラウザー独自の正規表現に依存するため、不適切なパターンは
//! コンパイルに失敗するだけで、エディターが中断されることはありません。
//! 構造も同様に行ごとに検索されます。一致とは、文書内のどこにあっても、
//! 互いに隣り合った一連の文字です。行の端にまたがることがないのと同じように、
//! 一致は構造の端にまたがることはありません。

use std::collections::HashMap;

use js_sys::RegExp;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

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

/// コンパイル済みの検索パターン。正規表現はブラウザーに任せ、それ以外は Boyer-Moore 法で検索します。
#[derive(Debug)]
enum Matcher {
    Regex(RegExp),
    Literal {
        pattern: Vec<char>,
        case_sensitive: bool,
        bad_char: HashMap<char, usize>,
        good_suffix: Vec<usize>,
    },
}

pub fn find_next(text: &Text, query: &str, options: SearchOptions, from: Key) -> Option<Found> {
    let all = find_all(text, query, options);
    all.iter()
        .find(|found| found.place.start() >= from)
        .or_else(|| all.first())
        .cloned()
}

pub fn find_all(text: &Text, query: &str, options: SearchOptions) -> Vec<Found> {
    let Some(matcher) = compile(query, options) else {
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

/// ブラウザーの正規表現を使って一致を探します。
fn regex_matches(regex: &RegExp, run: &str) -> Vec<(usize, usize, Vec<String>)> {
    let mut found = Vec::new();
    regex.set_last_index(0);
    let units: Vec<usize> = char_starts(run);
    loop {
        let Some(result) = regex.exec(run) else {
            break;
        };
        let Some(whole) = result.get(0).as_string() else {
            break;
        };
        let Some(at) = match_index(&result) else {
            break;
        };
        let end = at + whole.encode_utf16().count();
        if whole.is_empty() {
            // 空の一致は永遠にループします。
            regex.set_last_index(regex.last_index() + 1);
            if regex.last_index() as usize > run.encode_utf16().count() {
                break;
            }
            continue;
        }
        let (Some(from), Some(to)) = (char_of(&units, at), char_of(&units, end)) else {
            break;
        };
        let groups = (0..result.length())
            .map(|i| result.get(i).as_string().unwrap_or_default())
            .collect();
        found.push((from, to, groups));
        if !regex.global() {
            break;
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

/// ブラウザの正規表現でカウントされるため、各文字の UTF-16 インデックスが始まります。
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

/// 一致の開始位置は、ブラウザが報告する UTF-16 単位です。
fn match_index(result: &js_sys::Array<js_sys::JsString>) -> Option<usize> {
    js_sys::Reflect::get(result, &JsValue::from_str("index"))
        .ok()?
        .as_f64()
        .map(|index| index as usize)
}

fn char_of(starts: &[usize], unit: usize) -> Option<usize> {
    starts.iter().position(|start| *start == unit)
}

/// コンパイルクエリ。正規表現はブラウザーに、それ以外は Rust 側の Boyer-Moore 法に任せます。
fn compile(query: &str, options: SearchOptions) -> Option<Matcher> {
    if query.is_empty() {
        return None;
    }
    if options.regex {
        let source = query.to_string();
        let flags = if options.case_sensitive { "g" } else { "gi" };
        let make = js_sys::Function::new_with_args(
            "source, flags",
            "try { return new RegExp(source, flags); } catch (e) { return null; }",
        );
        make.call2(&JsValue::NULL, &source.into(), &flags.into())
            .ok()?
            .dyn_into::<RegExp>()
            .ok()
            .map(Matcher::Regex)
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

    /// リテラル検索は正規表現のメタ文字をただの文字として扱います。
    #[test]
    fn literal_queries_treat_metacharacters_as_text() {
        let text = Text::from_lines(vec![vec![
            Item::Char('a'),
            Item::Char('.'),
            Item::Char('b'),
            Item::Char('*'),
        ]]);
        let found = find_all(&text, "a.b*", SearchOptions::default());
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
        let found = find_all(&text, "aa", SearchOptions::default());
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
        let found = find_all(&text, "bc", SearchOptions::default());
        assert_eq!(found.len(), 1);
        assert!(matches!(found[0].place, Place::Inside { .. }));
    }
}
