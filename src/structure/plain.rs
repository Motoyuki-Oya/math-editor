//! ドキュメントの一部を通常の 1 次元テキストとして読み出す。
//!
//! これが外の世界に見えるものです。描画される前に書かれていたであろう構造が平らに置かれ、「c」の上の「a+b」が「(a+b)/c」になります。これはファイル形式ではありません。ここにあるものは読み戻すことができません。そのため、単純で短くすることは自由であり、表記法に邪魔されません。

use super::ast::{Between, MatrixKind, Node, Row};
use super::text::Item;
use super::vocabulary;

/// テキストの範囲の行。1 行に 1 つの文字列。
pub fn lines(lines: &[Vec<Item>]) -> String {
    lines
        .iter()
        .map(|items| items.iter().map(item).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

fn item(item: &Item) -> String {
    match item {
        Item::Char(c) => c.to_string(),
        Item::Tab => "\t".to_string(),
        Item::Math(root) => row(root),
    }
}

/// 構造の行。左から右に読みます。
pub fn row(row: &Row) -> String {
    row.iter().map(node).collect()
}

/// 他のものの横にある行。複数の場合は括弧で囲みます。`c` の上に `a+b` は次のようには読み取れません。 `a+b/c`.
fn part(part: &Row) -> String {
    let text = row(part);
    if part.len() > 1 {
        format!("({text})")
    } else {
        text
    }
}

fn node(node: &Node) -> String {
    match node {
        Node::Char(c) => c.to_string(),
        // 名前は、文字が存在する場合はどこでもその文字を表します。
        Node::Sym(name) => vocabulary::glyph_for(name)
            .unwrap_or(name.as_str())
            .to_string(),
        Node::Func(name) => name.clone(),
        Node::Stack {
            above,
            below,
            between,
        } => {
            let (above, below) = (part(above), part(below));
            match between {
                Between::Rule => format!("{above}/{below}"),
                Between::Arrow(arrow) => format!("{above}{arrow}{below}"),
                Between::Nothing => format!("{above} {below}"),
            }
        }
        Node::Sqrt { index, body } => match index {
            Some(index) => format!("{}√{}", part(index), part(body)),
            None => format!("√{}", part(body)),
        },
        Node::Sup(row) => format!("^{}", part(row)),
        Node::Sub(row) => format!("_{}", part(row)),
        Node::Group { delim, body } => {
            let (open, close) = delim.pair();
            format!("{open}{}{close}", row(body))
        }
        Node::Limits { sym, lower, upper } => {
            let sym = vocabulary::glyph_for(sym).unwrap_or(sym.as_str());
            let mut out = sym.to_string();
            if !lower.is_empty() {
                out.push('_');
                out.push_str(&part(lower));
            }
            if !upper.is_empty() {
                out.push('^');
                out.push_str(&part(upper));
            }
            out
        }
        // グリッドは、表がテキストを読み取るのと同じように読み取ります。つまり、セルを離し、行を離します。
        Node::Matrix { kind, cells } => {
            let rows = cells
                .iter()
                .map(|cells| cells.iter().map(row).collect::<Vec<_>>().join(", "))
                .collect::<Vec<_>>()
                .join("; ");
            match kind {
                MatrixKind::Grid => format!("[{rows}]"),
                MatrixKind::Cases => format!("{{{rows}}}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::structure::ast::{Delim, MatrixKind};

    fn stack(above: Row, below: Row) -> Node {
        Node::Stack {
            above,
            below,
            between: Between::Rule,
        }
    }

    fn chars(text: &str) -> Row {
        text.chars().map(Node::Char).collect()
    }

    #[test]
    fn a_fraction_reads_as_a_division() {
        assert_eq!(row(&vec![stack(chars("a"), chars("b"))]), "a/b");
    }

    #[test]
    fn a_part_of_more_than_one_thing_gets_brackets() {
        assert_eq!(row(&vec![stack(chars("a+b"), chars("c"))]), "(a+b)/c");
    }

    #[test]
    fn scripts_and_roots_read_the_way_they_are_typed() {
        assert_eq!(row(&vec![Node::Char('x'), Node::Sup(chars("2"))]), "x^2");
        assert_eq!(row(&vec![Node::Char('x'), Node::Sub(chars("i"))]), "x_i");
        assert_eq!(
            row(&vec![Node::Sqrt {
                index: None,
                body: chars("2")
            }]),
            "√2"
        );
    }

    #[test]
    fn a_name_reads_as_the_character_it_stands_for() {
        assert_eq!(row(&vec![Node::Sym("alpha".into())]), "α");
        assert_eq!(row(&vec![Node::Func("sin".into())]), "sin");
    }

    #[test]
    fn a_grid_reads_as_a_table() {
        let node = Node::Matrix {
            kind: MatrixKind::Grid,
            cells: vec![vec![chars("a"), chars("b")], vec![chars("c"), chars("d")]],
        };
        assert_eq!(row(&vec![node]), "[a, b; c, d]");
    }

    #[test]
    fn a_group_keeps_its_own_brackets() {
        let node = Node::Group {
            delim: Delim::Paren,
            body: chars("x"),
        };
        assert_eq!(row(&vec![node]), "(x)");
    }

    /// 列の区切り文字はタブであり、他の場所でも同様です。
    #[test]
    fn the_text_reads_with_its_structures_in_place() {
        let items = vec![vec![
            Item::Char('x'),
            Item::Tab,
            Item::Math(vec![stack(chars("1"), chars("2"))]),
        ]];
        assert_eq!(lines(&items), "x\t1/2");
    }
}
