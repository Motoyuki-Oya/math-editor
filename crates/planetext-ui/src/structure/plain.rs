//! ドキュメントの一部を通常の 1 次元テキストとして読み出す。
//!
//! これが外の世界に見えるものです。描画される前に書かれていたであろう構造が平らに置かれ、「c」の上の「a+b」が「(a+b)/c」になります。これはファイル形式ではありません。ここにあるものは読み戻すことができません。そのため、単純で短くすることは自由であり、表記法に邪魔されません。

use super::ast::{Between, MatrixKind, Node, NodeKind, Row};

/// テキストの範囲の行。1 行に 1 つの文字列。
pub fn lines(lines: &[Row]) -> String {
    lines.iter().map(|r| row(r)).collect::<Vec<_>>().join("\n")
}

/// 構造の行。左から右に読みます。
pub fn row(row: &[Node]) -> String {
    row.iter().map(node).collect()
}

/// 他のものの横にある行。複数の場合は括弧で囲みます。`c` の上に `a+b` は次のようには読み取れません。 `a+b/c`.
fn part(part: &[Node]) -> String {
    let text = row(part);
    if part.len() > 1 {
        format!("({text})")
    } else {
        text
    }
}

fn node(node: &Node) -> String {
    let mut out = match &node.kind {
        NodeKind::Char(c) => c.to_string(),
        NodeKind::Tab => "\t".to_string(),
        NodeKind::Stack {
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
        NodeKind::Sqrt { index, body } => match index {
            Some(index) => format!("{}√{}", part(index), part(body)),
            None => format!("√{}", part(body)),
        },
        NodeKind::Sup(row) => format!("^{}", part(row)),
        NodeKind::Sub(row) => format!("_{}", part(row)),
        NodeKind::Group { delim, body } => {
            let (open, close) = delim.pair();
            format!("{open}{}{close}", row(body))
        }
        NodeKind::BigOp(sym) => sym.clone(),
        NodeKind::Container(body) => row(body),
        // グリッドは、表がテキストを読み取るのと同じように読み取ります。つまり、セルを離し、行を離します。
        NodeKind::Matrix { kind, cells } => {
            let rows = cells
                .iter()
                .map(|cells| cells.iter().map(|c| row(c)).collect::<Vec<_>>().join(", "))
                .collect::<Vec<_>>()
                .join("; ");
            match kind {
                MatrixKind::Grid => format!("[{rows}]"),
                MatrixKind::Cases => format!("{{{rows}}}"),
            }
        }
    };
    if !node.lower.is_empty() {
        out.push('_');
        out.push_str(&part(&node.lower));
    }
    if !node.upper.is_empty() {
        out.push('^');
        out.push_str(&part(&node.upper));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::structure::ast::MatrixKind;

    fn stack(above: Row, below: Row) -> Node {
        Node::stack(above, below, Between::Rule)
    }

    fn chars(text: &str) -> Row {
        text.chars().map(Node::char).collect()
    }

    #[test]
    fn a_fraction_reads_as_a_division() {
        assert_eq!(row(&[stack(chars("a"), chars("b"))]), "a/b");
    }

    #[test]
    fn a_part_of_more_than_one_thing_gets_brackets() {
        assert_eq!(row(&[stack(chars("a+b"), chars("c"))]), "(a+b)/c");
    }

    #[test]
    fn scripts_and_roots_read_the_way_they_are_typed() {
        assert_eq!(row(&[Node::char('x'), Node::sup(chars("2"))]), "x^2");
        assert_eq!(row(&[Node::char('x'), Node::sub(chars("i"))]), "x_i");
        assert_eq!(row(&[Node::sqrt(None, chars("2"))]), "√2");
    }

    #[test]
    fn a_grid_reads_as_a_table() {
        let node = Node::matrix(
            MatrixKind::Grid,
            vec![vec![chars("a"), chars("b")], vec![chars("c"), chars("d")]],
        );
        assert_eq!(row(&[node]), "[a, b; c, d]");
    }

    /// 列の区切り文字はタブであり、他の場所でも同様です。
    #[test]
    fn the_text_reads_with_its_structures_in_place() {
        let nodes = vec![vec![
            Node::char('x'),
            Node::tab(),
            stack(chars("1"), chars("2")),
        ]];
        assert_eq!(lines(&nodes), "x\t1/2");
    }
}
