//! MathML export, used when writing an HTML copy of the document.

use super::ast::{Delim, MatrixKind, Node, Row};
use super::symbols::{self, Class};

pub fn to_mathml(row: &Row, display: bool) -> String {
    let mode = if display { "block" } else { "inline" };
    format!(
        "<math xmlns=\"http://www.w3.org/1998/Math/MathML\" display=\"{mode}\">{}</math>",
        row_to_mathml(row)
    )
}

pub fn row_to_mathml(row: &Row) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < row.len() {
        // A script node applies to the element before it, so the two have to be
        // emitted together as msup / msub / msubsup.
        let base = &row[i];
        let sub = script_at(row, i + 1, false);
        let sup = script_at(row, i + 1, true);
        let (sub, sup, consumed) = match (sub, sup) {
            (Some(sub), _) => match script_at(row, i + 2, true) {
                Some(sup) => (Some(sub), Some(sup), 3),
                None => (Some(sub), None, 2),
            },
            (None, Some(sup)) => match script_at(row, i + 2, false) {
                Some(sub) => (Some(sub), Some(sup), 3),
                None => (None, Some(sup), 2),
            },
            (None, None) => (None, None, 1),
        };
        let base_ml = node_to_mathml(base);
        out.push_str(&match (sub, sup) {
            (Some(sub), Some(sup)) => format!(
                "<msubsup>{base_ml}{}{}</msubsup>",
                wrap(&row_to_mathml(sub)),
                wrap(&row_to_mathml(sup))
            ),
            (Some(sub), None) => {
                format!("<msub>{base_ml}{}</msub>", wrap(&row_to_mathml(sub)))
            }
            (None, Some(sup)) => {
                format!("<msup>{base_ml}{}</msup>", wrap(&row_to_mathml(sup)))
            }
            (None, None) => base_ml,
        });
        i += consumed;
    }
    out
}

fn script_at(row: &Row, index: usize, sup: bool) -> Option<&Row> {
    match row.get(index) {
        Some(Node::Sup(inner)) if sup => Some(inner),
        Some(Node::Sub(inner)) if !sup => Some(inner),
        _ => None,
    }
}

fn wrap(inner: &str) -> String {
    format!("<mrow>{inner}</mrow>")
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn node_to_mathml(node: &Node) -> String {
    match node {
        Node::Char(c) => {
            let text = escape(&c.to_string());
            if c.is_ascii_digit() {
                format!("<mn>{text}</mn>")
            } else if c.is_alphabetic() {
                format!("<mi>{text}</mi>")
            } else {
                format!("<mo>{text}</mo>")
            }
        }
        Node::Sym(name) => {
            let symbol = symbols::lookup(name);
            let glyph = escape(symbol.map(|s| s.glyph).unwrap_or(name.as_str()));
            match symbol.map(|s| s.class) {
                Some(Class::Ident) => format!("<mi>{glyph}</mi>"),
                _ => format!("<mo>{glyph}</mo>"),
            }
        }
        Node::Func(name) => format!("<mi>{}</mi>", escape(name)),
        Node::Frac { num, den } => format!(
            "<mfrac>{}{}</mfrac>",
            wrap(&row_to_mathml(num)),
            wrap(&row_to_mathml(den))
        ),
        Node::Sqrt { index, body } => match index {
            Some(index) if !index.is_empty() => format!(
                "<mroot>{}{}</mroot>",
                wrap(&row_to_mathml(body)),
                wrap(&row_to_mathml(index))
            ),
            _ => format!("<msqrt>{}</msqrt>", row_to_mathml(body)),
        },
        // Reached only for a script without a base; emit the content as-is.
        Node::Sup(row) | Node::Sub(row) => wrap(&row_to_mathml(row)),
        Node::Group { delim, body } => {
            let (open, close) = match delim {
                Delim::Paren => ("(", ")"),
                Delim::Bracket => ("[", "]"),
                Delim::Brace => ("{", "}"),
                Delim::Bar => ("|", "|"),
            };
            format!(
                "<mrow><mo stretchy=\"true\">{open}</mo>{}<mo stretchy=\"true\">{close}</mo></mrow>",
                row_to_mathml(body)
            )
        }
        Node::BigOp { name, lower, upper } => {
            let op = symbols::big_op(name);
            let glyph = escape(op.map(|o| o.glyph).unwrap_or(name.as_str()));
            let base = format!("<mo>{glyph}</mo>");
            match (lower.is_empty(), upper.is_empty()) {
                (true, true) => base,
                (false, true) => format!("<munder>{base}{}</munder>", wrap(&row_to_mathml(lower))),
                (true, false) => format!("<mover>{base}{}</mover>", wrap(&row_to_mathml(upper))),
                (false, false) => format!(
                    "<munderover>{base}{}{}</munderover>",
                    wrap(&row_to_mathml(lower)),
                    wrap(&row_to_mathml(upper))
                ),
            }
        }
        Node::Matrix { kind, cells } => {
            let table = cells
                .iter()
                .map(|row| {
                    let tds = row
                        .iter()
                        .map(|cell| format!("<mtd>{}</mtd>", row_to_mathml(cell)))
                        .collect::<String>();
                    format!("<mtr>{tds}</mtr>")
                })
                .collect::<String>();
            let table = format!("<mtable>{table}</mtable>");
            match kind {
                MatrixKind::Plain => table,
                MatrixKind::Paren => format!(
                    "<mrow><mo stretchy=\"true\">(</mo>{table}<mo stretchy=\"true\">)</mo></mrow>"
                ),
                MatrixKind::Bracket => format!(
                    "<mrow><mo stretchy=\"true\">[</mo>{table}<mo stretchy=\"true\">]</mo></mrow>"
                ),
                MatrixKind::Cases => {
                    format!("<mrow><mo stretchy=\"true\">{{</mo>{table}</mrow>")
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::latex::parse_latex;
    use super::*;

    #[test]
    fn fraction_becomes_mfrac() {
        let ml = row_to_mathml(&parse_latex("\\frac{1}{2}"));
        assert_eq!(
            ml,
            "<mfrac><mrow><mn>1</mn></mrow><mrow><mn>2</mn></mrow></mfrac>"
        );
    }

    #[test]
    fn root_with_index_becomes_mroot() {
        let ml = row_to_mathml(&parse_latex("\\sqrt[3]{x}"));
        assert!(ml.starts_with("<mroot>"));
    }

    #[test]
    fn scripts_attach_to_their_base() {
        assert_eq!(
            row_to_mathml(&parse_latex("x^{2}")),
            "<msup><mi>x</mi><mrow><mn>2</mn></mrow></msup>"
        );
        assert!(row_to_mathml(&parse_latex("x_i^2")).starts_with("<msubsup>"));
    }

    #[test]
    fn big_operator_limits_become_munderover() {
        let ml = row_to_mathml(&parse_latex("\\sum_{i=1}^{n}"));
        assert!(ml.starts_with("<munderover>"));
    }

    #[test]
    fn markup_characters_are_escaped() {
        assert_eq!(
            row_to_mathml(&parse_latex("a<b")),
            "<mi>a</mi><mo>&lt;</mo><mi>b</mi>"
        );
    }
}
