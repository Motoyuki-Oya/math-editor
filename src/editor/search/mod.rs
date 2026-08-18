//! オプションの正規表現を使用して、モデルを検索して置換します。
//!
//! 通常の（正規表現でない）検索は Rust 側で Boyer-Moore 法を使います。
//! 長大な文書や正規表現の検索には `regex` クレートを使用します。
//! 不適切なパターンはコンパイルに失敗するだけで、エディターが中断されることはありません。
//! 構造も同様に行ごとに検索されます。一致とは、文書内のどこにあっても、
//! 互いに隣り合った一連の文字です。行の端にまたがることがないのと同じように、
//! 一致は構造の端にまたがることはありません。

use crate::structure::ast::{Cursor, Node, Row};
use crate::structure::text::{Item, Pos, Sel, Text};

mod boyer_moore;
mod matcher;

use matcher::{compile, Matcher};

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
            for (from, to, groups) in matcher.matches(&run) {
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
        for (from, to, groups) in matcher.matches(&run) {
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

    /// リテラル検索は正規表現のメタ文字をただの文字として扱います。
    #[test]
    fn literal_queries_treat_metacharacters_as_text() {
        let text = Text::from_lines(vec![vec![
            Item::Char('a'),
            Item::Char('.'),
            Item::Char('b'),
            Item::Char('*'),
        ]]);
        let found = find_all(&text, "a.b*", SearchOptions::default(), Some(0));
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
            Some(0),
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

    /// 構造内の文字もリテラル検索で見つかります。
    #[test]
    fn literal_search_finds_characters_inside_structures() {
        let text = Text::from_lines(vec![vec![
            Item::Char('a'),
            Item::Math(vec![Node::Char('b'), Node::Char('c')]),
            Item::Char('d'),
        ]]);
        let found = find_all(&text, "bc", SearchOptions::default(), Some(0));
        assert_eq!(found.len(), 1);
        assert!(matches!(found[0].place, Place::Inside { .. }));
    }
}
