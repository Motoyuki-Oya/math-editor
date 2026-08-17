//! `docs/notation.md` で説明されているように、アイランドが記述される表記法です。
//!
//! `$(` … `)` 内のすべてがここで読み取られ、ここで書き戻されるため、保存されたファイルはプレーン テキストのままであり、他の形式は関与しません。

use crate::structure::ast::{is_arrow, Between, Delim, MatrixKind, Node, Row};
use crate::structure::vocabulary;

pub const LIMITS_MARK: char = '↨';
pub const ROOT_MARK: char = '√';
pub const ROOT_WORD: &str = "sqrt";

/// 構造的に何かを意味する文字なので、1 つを通常の文字として書くことは 2 回書くことになります。
const SPECIAL: [char; 7] = ['$', '/', '-', ',', ';', '[', ']'];

fn is_special(c: char) -> bool {
    SPECIAL.contains(&c) || is_arrow(c)
}

// ---------------------------------------------------------------- reading

/// アイランドの内容を読み取ります (テキスト
pub fn parse_island(src: &str) -> Row {
    let chars: Vec<char> = src.chars().collect();
    content(&chars)
}

fn trim(chars: &[char]) -> &[char] {
    let start = chars
        .iter()
        .position(|c| !c.is_whitespace())
        .unwrap_or(chars.len());
    let end = chars
        .iter()
        .rposition(|c| !c.is_whitespace())
        .map(|i| i + 1)
        .unwrap_or(start);
    &chars[start..end]
}

fn starts_with(chars: &[char], word: &str) -> bool {
    chars
        .iter()
        .copied()
        .take(word.chars().count())
        .eq(word.chars())
}

/// `chars` の最上位にある構造文字の位置: ネストされたアイランド、グループ、または行内ではなく、二重化されていません。
fn top_level(chars: &[char]) -> Vec<(usize, char)> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if is_special(c) && chars.get(i + 1) == Some(&c) {
            i += 2;
            continue;
        }
        match c {
            '$' if chars.get(i + 1) == Some(&'(') => {
                depth += 1;
                i += 2;
                continue;
            }
            '(' | '[' => {
                if depth == 0 {
                    out.push((i, c));
                }
                depth += 1;
            }
            ')' | ']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    out.push((i, c));
                }
            }
            _ if depth == 0 && (is_special(c) || c == LIMITS_MARK) => out.push((i, c)),
            _ => {}
        }
        i += 1;
    }
    out
}

/// 最上位のカンマで分割されるため、`Σ, n, x=1` は 3 つの引数になり、`lim,, n→∞` は中央の引数を省略します。
fn split_args(chars: &[char]) -> Vec<&[char]> {
    let mut args = Vec::new();
    let mut start = 0;
    let mut depth = 0usize;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c != ',' && is_special(c) && chars.get(i + 1) == Some(&c) {
            i += 2;
            continue;
        }
        match c {
            '$' if chars.get(i + 1) == Some(&'(') => {
                depth += 1;
                i += 2;
                continue;
            }
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                args.push(trim(&chars[start..i]));
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    args.push(trim(&chars[start..]));
    args
}

fn arg(args: &[&[char]], i: usize) -> Row {
    args.get(i).map(|a| row(a)).unwrap_or_default()
}

/// アイランド全体を読み取ります: 最も外側の構造
fn content(chars: &[char]) -> Row {
    let chars = trim(chars);
    if chars.is_empty() {
        return Row::new();
    }
    match chars[0] {
        c if is_stack_mark(c) && chars.get(1) != Some(&c) => {
            let args = split_args(trim(&chars[1..]));
            return vec![Node::Stack {
                above: arg(&args, 0),
                below: arg(&args, 1),
                between: between(c),
            }];
        }
        LIMITS_MARK => {
            let args = split_args(trim(&chars[1..]));
            let sym: String = args.first().map(|a| a.iter().collect()).unwrap_or_default();
            return vec![Node::Limits {
                sym,
                upper: arg(&args, 1),
                lower: arg(&args, 2),
            }];
        }
        '^' | '_' => return scripts(chars),
        '[' => return vec![matrix(chars, MatrixKind::Grid)],
        '{' => return vec![matrix(trim(&chars[1..]), MatrixKind::Cases)],
        _ => {}
    }
    if starts_with(chars, ROOT_WORD) || chars[0] == ROOT_MARK {
        let rest = if chars[0] == ROOT_MARK {
            &chars[1..]
        } else {
            &chars[ROOT_WORD.chars().count()..]
        };
        return root(rest);
    }
    if let Some((i, c)) = top_level(chars)
        .into_iter()
        .find(|&(_, c)| is_stack_mark(c))
    {
        return vec![Node::Stack {
            above: row(trim(&chars[..i])),
            below: row(trim(&chars[i + 1..])),
            between: between(c),
        }];
    }
    row(chars)
}

/// スタックの 2 行を区切る文字。
fn is_stack_mark(c: char) -> bool {
    c == '/' || c == '-' || is_arrow(c)
}

fn between(mark: char) -> Between {
    match mark {
        '/' => Between::Rule,
        '-' => Between::Nothing,
        arrow => Between::Arrow(arrow),
    }
}

/// `√[n] x`: インデックスはオプションで、本文はそれに続く 1 つのチャンクであり、その後に来るものはルートの下ではなく横にあります。
fn root(chars: &[char]) -> Row {
    let chars = trim(chars);
    let (index, rest) = match chars.first() {
        Some('[') if chars.get(1) != Some(&'[') => match closing(chars, 0) {
            Some(end) => (Some(row(trim(&chars[1..end]))), &chars[end + 1..]),
            None => (None, chars),
        },
        _ => (None, chars),
    };
    let rest = trim(rest);
    let (body, taken) = chunk(rest);
    let mut out = vec![Node::Sqrt { index, body }];
    out.extend(row(&rest[taken.min(rest.len())..]));
    out
}

/// 1 つのチャンク: 文字、グループ、またはネストされたアイランド、およびそれに関連付けられたスクリプト。また、使用した `chars` の量もレポートします。
fn chunk(chars: &[char]) -> (Row, usize) {
    let mut out = Row::new();
    let mut i = 0;
    if let Some((node, next)) = node_at(chars, 0) {
        out.push(node);
        i = next;
    }
    while let Some((node, next)) = node_at(chars, i) {
        if !matches!(node, Node::Sup(_) | Node::Sub(_)) {
            break;
        }
        out.push(node);
        i = next;
    }
    (out, i)
}

/// `^ 3 _ i`: 各マーカーは次のマーカーまでのすべてを取得します。
fn scripts(chars: &[char]) -> Row {
    let marks: Vec<usize> = chars
        .iter()
        .enumerate()
        .filter(|&(i, &c)| (c == '^' || c == '_') && depth_at(chars, i) == 0)
        .map(|(i, _)| i)
        .collect();
    let mut out = Row::new();
    for (n, &start) in marks.iter().enumerate() {
        let end = marks.get(n + 1).copied().unwrap_or(chars.len());
        let body = row(trim(&chars[start + 1..end]));
        out.push(if chars[start] == '^' {
            Node::Sup(body)
        } else {
            Node::Sub(body)
        });
    }
    out
}

fn depth_at(chars: &[char], at: usize) -> usize {
    let mut depth = 0usize;
    let mut i = 0;
    while i < at.min(chars.len()) {
        let c = chars[i];
        if is_special(c) && chars.get(i + 1) == Some(&c) {
            i += 2;
            continue;
        }
        match c {
            '$' if chars.get(i + 1) == Some(&'(') => {
                depth += 1;
                i += 1;
            }
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            _ => {}
        }
        i += 1;
    }
    depth
}

/// `[a, b][c, d]`: 行ごとに 1 つの括弧で囲まれたグループ、セル間にカンマ。
fn matrix(chars: &[char], kind: MatrixKind) -> Node {
    let mut cells: Vec<Vec<Row>> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '[' {
            i += 1;
            continue;
        }
        let Some(end) = closing(chars, i) else { break };
        cells.push(
            split_args(&chars[i + 1..end])
                .into_iter()
                .map(row)
                .collect(),
        );
        i = end + 1;
    }
    if cells.is_empty() {
        cells.push(vec![Row::new()]);
    }
    let width = cells.iter().map(|r| r.len()).max().unwrap_or(1);
    for row in &mut cells {
        row.resize_with(width, Row::new);
    }
    Node::Matrix { kind, cells }
}

/// `open` の括弧または括弧を閉じる括弧または括弧のインデックス。
fn closing(chars: &[char], open: usize) -> Option<usize> {
    let close = match chars.get(open)? {
        '[' => ']',
        '(' => ')',
        _ => return None,
    };
    let mut depth = 0usize;
    let mut i = open;
    while i < chars.len() {
        let c = chars[i];
        if is_special(c) && chars.get(i + 1) == Some(&c) {
            i += 2;
            continue;
        }
        match c {
            '$' if chars.get(i + 1) == Some(&'(') => {
                depth += 1;
                i += 1;
            }
            '(' | '[' => depth += 1,
            c if c == close || c == ')' || c == ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// 単純なシーケンス: 通常の文字、グループ、およびネストされたIslands.
fn row(chars: &[char]) -> Row {
    let mut out = Row::new();
    let mut i = 0;
    while let Some((node, next)) = node_at(chars, i) {
        out.push(node);
        i = next;
    }
    out
}

/// `i` で始まるノードと、読み取りが停止した場所を読み取ります。
fn node_at(chars: &[char], i: usize) -> Option<(Node, usize)> {
    let c = *chars.get(i)?;
    if is_special(c) && chars.get(i + 1) == Some(&c) {
        return Some((Node::Char(c), i + 2));
    }
    if c == '$' && chars.get(i + 1) == Some(&'(') {
        let end = island_end(chars, i)?;
        let inner = content(&chars[i + 2..end]);
        let node = match inner.len() {
            1 => inner.into_iter().next()?,
            _ => Node::Group {
                delim: Delim::Paren,
                body: inner,
            },
        };
        return Some((node, end + 1));
    }
    if c == '(' {
        let end = closing(chars, i)?;
        return Some((
            Node::Group {
                delim: Delim::Paren,
                body: row(&chars[i + 1..end]),
            },
            end + 1,
        ));
    }
    if c == '[' {
        let end = closing(chars, i)?;
        return Some((
            Node::Group {
                delim: Delim::Bracket,
                body: row(&chars[i + 1..end]),
            },
            end + 1,
        ));
    }
    Some((Node::Char(c), i + 1))
}

/// `at` で始まるアイランドを閉じる `)` のインデックス。
pub fn island_end(chars: &[char], at: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = at;
    while i < chars.len() {
        let c = chars[i];
        if is_special(c) && chars.get(i + 1) == Some(&c) {
            i += 2;
            continue;
        }
        match c {
            '$' if chars.get(i + 1) == Some(&'(') => {
                depth += 1;
                i += 2;
                continue;
            }
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

// ---------------------------------------------------------------- 書き込み

/// アイランドの内容として行を書き込みます。最も外側の単一の構造は裸で書き込まれ、それ以外はすべて独自のアイランドを取得します。
pub fn island_text(root: &Row) -> String {
    match root.as_slice() {
        [node] if node.slot_count() > 0 => bare(node),
        nodes => nodes.iter().map(inline).collect(),
    }
}

/// 行内に表示されるノード: 構造には独自のアイランドが必要です。
fn inline(node: &Node) -> String {
    match node {
        Node::Char(c) if is_special(*c) => format!("{c}{c}"),
        Node::Char(c) => c.to_string(),
        Node::Sym(name) => vocabulary::lookup(name)
            .map(|s| s.glyph.to_string())
            .unwrap_or_else(|| name.clone()),
        Node::Func(name) => name.clone(),
        Node::Group { delim, body } => {
            let (open, close) = delim.pair();
            format!("{open}{}{close}", text(body))
        }
        other => format!("$({})", bare(other)),
    }
}

fn text(row: &Row) -> String {
    row.iter().map(inline).collect()
}

/// A
fn bare(node: &Node) -> String {
    match node {
        Node::Stack {
            above,
            below,
            between,
        } => {
            let (above, below) = (text(above), text(below));
            match between {
                Between::Rule => format!("{above}/{below}"),
                Between::Nothing => format!("{above} - {below}"),
                Between::Arrow(arrow) => format!("{above} {arrow} {below}"),
            }
        }
        Node::Sqrt { index, body } => match index {
            Some(index) => format!("{ROOT_MARK}[{}] {}", text(index), text(body)),
            None => format!("{ROOT_MARK} {}", text(body)),
        },
        Node::Sup(body) => format!("^ {}", text(body)),
        Node::Sub(body) => format!("_ {}", text(body)),
        Node::Limits { sym, lower, upper } => {
            format!("{LIMITS_MARK} {sym}, {}, {}", text(upper), text(lower))
        }
        Node::Matrix { kind, cells } => {
            let rows: String = cells
                .iter()
                .map(|row| {
                    let cells: Vec<String> = row.iter().map(text).collect();
                    format!("[{}]", cells.join(", "))
                })
                .collect();
            match kind {
                MatrixKind::Grid => rows,
                MatrixKind::Cases => format!("{{{rows}"),
            }
        }
        other => inline(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(src: &str) -> String {
        island_text(&parse_island(src))
    }

    #[test]
    fn a_rule_with_something_above_and_below() {
        assert_eq!(roundtrip("1/2"), "1/2");
        assert_eq!(roundtrip("/ x+1, 2y"), "x+1/2y");
        assert!(matches!(
            parse_island("1/2").as_slice(),
            [Node::Stack {
                between: Between::Rule,
                ..
            }]
        ));
    }

    #[test]
    fn a_stack_without_a_rule() {
        assert_eq!(roundtrip("n - k"), "n - k");
        assert_eq!(roundtrip("- n, k"), "n - k");
        assert!(matches!(
            parse_island("- n, k").as_slice(),
            [Node::Stack {
                between: Between::Nothing,
                ..
            }]
        ));
    }

    #[test]
    fn an_arrow_may_be_drawn_between_the_rows() {
        assert_eq!(roundtrip("→ f, g"), "f → g");
        assert_eq!(roundtrip("n→∞"), "n → ∞");
        match parse_island("→ f,").as_slice() {
            [Node::Stack { between, below, .. }] => {
                assert_eq!(*between, Between::Arrow('→'));
                assert!(below.is_empty());
            }
            other => panic!("unexpected {other:?}"),
        }
        // 二重にすると、また普通の矢印になります。
        assert_eq!(parse_island("a→→b").len(), 3);
    }

    #[test]
    fn roots_take_one_chunk() {
        assert_eq!(roundtrip("√ x"), "√ x");
        assert_eq!(roundtrip("sqrt[3] x"), "√[3] x");
        // マークの直後のチャンクだけがルートの下にあります。
        match parse_island("√ x+1").as_slice() {
            [Node::Sqrt { body, .. }, Node::Char('+'), Node::Char('1')] => {
                assert_eq!(body, &vec![Node::Char('x')]);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn a_bracketed_chunk_goes_under_the_root_whole() {
        match parse_island("√ (x+1)").as_slice() {
            [Node::Sqrt { body, .. }] => assert_eq!(body.len(), 1),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn scripts_can_be_written_together() {
        assert_eq!(roundtrip("^ 3"), "^ 3");
        let row = parse_island("^ 3 _ i");
        assert!(matches!(row.as_slice(), [Node::Sup(_), Node::Sub(_)]));
        assert_eq!(island_text(&row), "$(^ 3)$(_ i)");
    }

    #[test]
    fn a_symbol_carries_what_is_above_and_below_it() {
        assert_eq!(roundtrip("↨ Σ, n, x=1"), "↨ Σ, n, x=1");
        match parse_island("↨ Σ, n, x=1").as_slice() {
            [Node::Limits { sym, upper, lower }] => {
                assert_eq!(sym, "Σ");
                assert_eq!(text(upper), "n");
                assert_eq!(text(lower), "x=1");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn one_limit_may_be_left_out() {
        match parse_island("↨ lim,, n→∞").as_slice() {
            [Node::Limits { upper, lower, .. }] => {
                assert!(upper.is_empty());
                // ここでは矢印は普通の文字なので二重になります。
                assert_eq!(text(lower), "n→→∞");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn grids_and_case_splits() {
        assert_eq!(roundtrip("[a, b][c, d]"), "[a, b][c, d]");
        assert_eq!(roundtrip("{[x>0, 正][x<0, 負]"), "{[x>0, 正][x<0, 負]");
        match parse_island("[a, b][c, d]").as_slice() {
            [Node::Matrix { kind, cells }] => {
                assert_eq!(*kind, MatrixKind::Grid);
                assert_eq!(cells.len(), 2);
                assert_eq!(cells[0].len(), 2);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn islands_nest() {
        let row = parse_island("√ x$(^ 3)");
        match row.as_slice() {
            [Node::Sqrt { body, .. }] => {
                assert!(matches!(body.as_slice(), [Node::Char('x'), Node::Sup(_)]));
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(island_text(&row), "√ x$(^ 3)");
    }

    #[test]
    fn doubled_characters_are_ordinary_ones() {
        assert_eq!(
            parse_island("a//b"),
            vec![Node::Char('a'), Node::Char('/'), Node::Char('b')]
        );
        assert_eq!(roundtrip("a//b"), "a//b");
        assert_eq!(parse_island("a--b").len(), 3);
    }

    #[test]
    fn spaces_around_separators_do_not_matter() {
        assert_eq!(parse_island("↨Σ,n,x=1"), parse_island("↨ Σ, n, x=1"));
        assert_eq!(parse_island("√x"), parse_island("√ x"));
    }

    #[test]
    fn unfinished_input_does_not_panic() {
        let _ = parse_island("√[3");
        let _ = parse_island("$(");
        let _ = parse_island("[a, b");
    }
}
