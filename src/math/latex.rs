//! LaTeX is the on-disk form of a formula: everything is written out as LaTeX
//! when saving and read back when loading, so the user never has to type it.

use super::ast::{Delim, MatrixKind, Node, Row};
use super::symbols;

pub fn row_to_latex(row: &Row) -> String {
    let mut out = String::new();
    for node in row {
        node_to_latex(node, &mut out);
    }
    out
}

fn braced(row: &Row) -> String {
    format!("{{{}}}", row_to_latex(row))
}

fn node_to_latex(node: &Node, out: &mut String) {
    match node {
        Node::Char(c) => match c {
            '{' | '}' | '%' | '&' | '#' | '_' | '$' => {
                out.push('\\');
                out.push(*c);
            }
            _ => out.push(*c),
        },
        Node::Sym(name) => {
            out.push('\\');
            out.push_str(name);
            out.push(' ');
        }
        Node::Func(name) => {
            out.push('\\');
            out.push_str(name);
            out.push(' ');
        }
        Node::Frac { num, den } => {
            out.push_str("\\frac");
            out.push_str(&braced(num));
            out.push_str(&braced(den));
        }
        Node::Sqrt { index, body } => {
            out.push_str("\\sqrt");
            if let Some(index) = index {
                if !index.is_empty() {
                    out.push('[');
                    out.push_str(&row_to_latex(index));
                    out.push(']');
                }
            }
            out.push_str(&braced(body));
        }
        Node::Sup(row) => {
            out.push('^');
            out.push_str(&braced(row));
        }
        Node::Sub(row) => {
            out.push('_');
            out.push_str(&braced(row));
        }
        Node::Group { delim, body } => {
            let (open, close) = delim.latex();
            out.push_str("\\left");
            out.push_str(open);
            out.push_str(&row_to_latex(body));
            out.push_str("\\right");
            out.push_str(close);
        }
        Node::BigOp { name, lower, upper } => {
            out.push('\\');
            out.push_str(name);
            if !lower.is_empty() {
                out.push('_');
                out.push_str(&braced(lower));
            }
            if !upper.is_empty() {
                out.push('^');
                out.push_str(&braced(upper));
            }
            if lower.is_empty() && upper.is_empty() {
                out.push(' ');
            }
        }
        Node::Matrix { kind, cells } => {
            let env = kind.env();
            out.push_str("\\begin{");
            out.push_str(env);
            out.push('}');
            let body = cells
                .iter()
                .map(|row| row.iter().map(row_to_latex).collect::<Vec<_>>().join(" & "))
                .collect::<Vec<_>>()
                .join(" \\\\ ");
            out.push_str(&body);
            out.push_str("\\end{");
            out.push_str(env);
            out.push('}');
        }
    }
}

struct Parser<'a> {
    src: &'a [char],
    pos: usize,
}

/// Parses the subset of LaTeX this editor can produce. Anything unrecognised is
/// kept as literal characters rather than dropped, so no content is ever lost.
pub fn parse_latex(input: &str) -> Row {
    let chars: Vec<char> = input.chars().collect();
    let mut parser = Parser {
        src: &chars,
        pos: 0,
    };
    parser.parse_row(&[])
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<char> {
        self.src.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn skip_spaces(&mut self) {
        while matches!(self.peek(), Some(c) if c.is_whitespace()) {
            self.pos += 1;
        }
    }

    fn starts_with(&self, text: &str) -> bool {
        self.src[self.pos..]
            .iter()
            .zip(text.chars())
            .filter(|(a, b)| **a == *b)
            .count()
            == text.chars().count()
            && self.src.len() - self.pos >= text.chars().count()
    }

    fn eat(&mut self, text: &str) -> bool {
        if self.starts_with(text) {
            self.pos += text.chars().count();
            true
        } else {
            false
        }
    }

    /// Reads a command name (the letters after a backslash).
    fn command(&mut self) -> String {
        let start = self.pos;
        while matches!(self.peek(), Some(c) if c.is_ascii_alphabetic()) {
            self.pos += 1;
        }
        if start == self.pos {
            // A single non-letter escape such as `\{` or `\\`.
            if let Some(c) = self.bump() {
                return c.to_string();
            }
        }
        self.src[start..self.pos].iter().collect()
    }

    /// Parses until one of `stop` (or end of input) is reached.
    fn parse_row(&mut self, stop: &[&str]) -> Row {
        let mut row = Row::new();
        loop {
            self.skip_spaces();
            if self.peek().is_none() {
                break;
            }
            if stop.iter().any(|s| self.starts_with(s)) {
                break;
            }
            match self.parse_node() {
                Some(node) => row.push(node),
                None => break,
            }
        }
        row
    }

    /// Parses one `{...}` argument, or a single node when unbraced.
    fn argument(&mut self) -> Row {
        self.skip_spaces();
        if self.eat("{") {
            let row = self.parse_row(&["}"]);
            self.eat("}");
            row
        } else {
            match self.parse_node() {
                Some(node) => vec![node],
                None => Row::new(),
            }
        }
    }

    fn parse_node(&mut self) -> Option<Node> {
        let c = self.peek()?;
        match c {
            '\\' => {
                self.pos += 1;
                let name = self.command();
                Some(self.command_node(&name))
            }
            '^' => {
                self.pos += 1;
                Some(Node::Sup(self.argument()))
            }
            '_' => {
                self.pos += 1;
                Some(Node::Sub(self.argument()))
            }
            '(' | '[' => {
                self.pos += 1;
                let close = if c == '(' { ")" } else { "]" };
                let body = self.parse_row(&[close]);
                self.eat(close);
                Some(Node::Group {
                    delim: Delim::from_open(c).unwrap(),
                    body,
                })
            }
            '{' => {
                // A bare brace group only carries grouping; inline its content.
                self.pos += 1;
                let body = self.parse_row(&["}"]);
                self.eat("}");
                Some(Node::Group {
                    delim: Delim::Brace,
                    body,
                })
            }
            _ => {
                self.pos += 1;
                Some(Node::Char(c))
            }
        }
    }

    fn command_node(&mut self, name: &str) -> Node {
        match name {
            "frac" | "dfrac" | "tfrac" => Node::Frac {
                num: self.argument(),
                den: self.argument(),
            },
            "sqrt" => {
                self.skip_spaces();
                let index = if self.eat("[") {
                    let row = self.parse_row(&["]"]);
                    self.eat("]");
                    Some(row)
                } else {
                    None
                };
                Node::Sqrt {
                    index,
                    body: self.argument(),
                }
            }
            "left" => {
                self.skip_spaces();
                let open = self.read_delim();
                let body = self.parse_row(&["\\right"]);
                self.eat("\\right");
                self.skip_spaces();
                let _ = self.read_delim();
                Node::Group {
                    delim: open.unwrap_or(Delim::Paren),
                    body,
                }
            }
            "begin" => self.environment(),
            _ if symbols::big_op(name).is_some() => self.big_op(name),
            _ if symbols::is_function(name) => Node::Func(name.to_string()),
            _ if symbols::lookup(name).is_some() => Node::Sym(name.to_string()),
            // Unknown command: keep the characters so nothing is lost.
            _ => Node::Char(name.chars().next().unwrap_or('?')),
        }
    }

    fn read_delim(&mut self) -> Option<Delim> {
        if self.eat("\\{") {
            return Some(Delim::Brace);
        }
        if self.eat("\\}") {
            return Some(Delim::Brace);
        }
        match self.peek() {
            Some(c @ ('(' | '[' | '|')) => {
                self.pos += 1;
                Delim::from_open(c)
            }
            Some(')' | ']') => {
                self.pos += 1;
                None
            }
            _ => None,
        }
    }

    fn big_op(&mut self, name: &str) -> Node {
        let mut lower = Row::new();
        let mut upper = Row::new();
        loop {
            self.skip_spaces();
            match self.peek() {
                Some('_') if lower.is_empty() => {
                    self.pos += 1;
                    lower = self.argument();
                }
                Some('^') if upper.is_empty() => {
                    self.pos += 1;
                    upper = self.argument();
                }
                _ => break,
            }
        }
        Node::BigOp {
            name: name.to_string(),
            lower,
            upper,
        }
    }

    fn environment(&mut self) -> Node {
        self.eat("{");
        let start = self.pos;
        while matches!(self.peek(), Some(c) if c != '}') {
            self.pos += 1;
        }
        let env: String = self.src[start..self.pos].iter().collect();
        self.eat("}");
        let kind = MatrixKind::from_env(&env).unwrap_or(MatrixKind::Plain);
        let end = format!("\\end{{{env}}}");
        let mut cells: Vec<Vec<Row>> = vec![vec![]];
        loop {
            self.skip_spaces();
            if self.peek().is_none() || self.starts_with(&end) {
                break;
            }
            let cell = self.parse_row(&["&", "\\\\", &end]);
            cells.last_mut().unwrap().push(cell);
            self.skip_spaces();
            if self.eat("\\\\") {
                cells.push(vec![]);
            } else if self.eat("&") {
                continue;
            } else {
                break;
            }
        }
        self.eat(&end);
        if cells.last().is_some_and(|r| r.is_empty()) {
            cells.pop();
        }
        if cells.is_empty() {
            cells.push(vec![Row::new()]);
        }
        // Pad short rows so the grid stays rectangular.
        let width = cells.iter().map(|r| r.len()).max().unwrap_or(1);
        for row in &mut cells {
            row.resize_with(width, Row::new);
        }
        Node::Matrix { kind, cells }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(src: &str) -> String {
        row_to_latex(&parse_latex(src))
    }

    #[test]
    fn fractions_roundtrip() {
        assert_eq!(roundtrip("\\frac{1}{2}"), "\\frac{1}{2}");
        assert_eq!(roundtrip("\\frac{a+b}{2}"), "\\frac{a+b}{2}");
    }

    #[test]
    fn roots_roundtrip() {
        assert_eq!(roundtrip("\\sqrt{x}"), "\\sqrt{x}");
        assert_eq!(roundtrip("\\sqrt[3]{x+1}"), "\\sqrt[3]{x+1}");
    }

    #[test]
    fn scripts_roundtrip() {
        assert_eq!(roundtrip("x^{2}"), "x^{2}");
        assert_eq!(roundtrip("a_{ij}"), "a_{ij}");
    }

    #[test]
    fn big_operators_keep_limits() {
        let row = parse_latex("\\sum_{i=1}^{n}i");
        assert_eq!(row_to_latex(&row), "\\sum_{i=1}^{n}i");
    }

    #[test]
    fn symbols_and_functions_are_recognised() {
        let row = parse_latex("\\sin \\theta \\leq 1");
        assert_eq!(row.len(), 4);
        assert!(matches!(row[0], Node::Func(_)));
        assert!(matches!(row[1], Node::Sym(_)));
        assert!(matches!(row[2], Node::Sym(_)));
    }

    #[test]
    fn matrices_roundtrip() {
        let latex = "\\begin{pmatrix}a & b \\\\ c & d\\end{pmatrix}";
        let row = parse_latex(latex);
        match &row[0] {
            Node::Matrix { kind, cells } => {
                assert_eq!(kind.env(), "pmatrix");
                assert_eq!(cells.len(), 2);
                assert_eq!(cells[0].len(), 2);
            }
            other => panic!("expected a matrix, got {other:?}"),
        }
        assert_eq!(row_to_latex(&row), latex);
    }

    #[test]
    fn delimiters_roundtrip() {
        assert_eq!(roundtrip("\\left(x+1\\right)"), "\\left(x+1\\right)");
    }

    #[test]
    fn unknown_commands_do_not_panic() {
        let _ = parse_latex("\\somethingweird{x}");
    }
}
