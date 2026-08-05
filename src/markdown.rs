//! The file format: plain text with formulas written as `$...$` (inline) and
//! `$$...$$` (own line). A literal dollar sign is escaped as `\$`.

use crate::math::latex::parse_latex;
use crate::math::mathml;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Segment {
    Text(String),
    Math { latex: String, display: bool },
}

pub type Line = Vec<Segment>;

pub fn escape_text(text: &str) -> String {
    text.replace('\\', "\\\\").replace('$', "\\$")
}

fn unescape_text(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some(next) => out.push(next),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub fn parse(text: &str) -> Vec<Line> {
    text.split('\n').map(parse_line).collect()
}

fn parse_line(line: &str) -> Line {
    let chars: Vec<char> = line.chars().collect();
    let mut segments = Line::new();
    let mut text = String::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\\' if i + 1 < chars.len() => {
                text.push('\\');
                text.push(chars[i + 1]);
                i += 2;
            }
            '$' => {
                let display = chars.get(i + 1) == Some(&'$');
                let open = if display { 2 } else { 1 };
                let close = if display { "$$" } else { "$" };
                match find_close(&chars, i + open, close) {
                    Some(end) => {
                        if !text.is_empty() {
                            segments.push(Segment::Text(unescape_text(&text)));
                            text.clear();
                        }
                        segments.push(Segment::Math {
                            latex: chars[i + open..end].iter().collect(),
                            display,
                        });
                        i = end + open;
                    }
                    // An unmatched `$` is just a dollar sign.
                    None => {
                        text.push('$');
                        i += 1;
                    }
                }
            }
            c => {
                text.push(c);
                i += 1;
            }
        }
    }
    if !text.is_empty() {
        segments.push(Segment::Text(unescape_text(&text)));
    }
    segments
}

fn find_close(chars: &[char], from: usize, close: &str) -> Option<usize> {
    let close: Vec<char> = close.chars().collect();
    let mut i = from;
    while i + close.len() <= chars.len() {
        if chars[i] == '\\' {
            i += 2;
            continue;
        }
        if chars[i..i + close.len()] == close[..] {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Renders the document as standalone HTML, with formulas as MathML so the
/// result stays readable in any browser without extra scripts.
pub fn to_html(text: &str, title: &str) -> String {
    let mut body = String::new();
    for line in parse(text) {
        let mut rendered = String::new();
        for segment in line {
            match segment {
                Segment::Text(text) => rendered.push_str(&escape_html(&text)),
                Segment::Math { latex, display } => {
                    rendered.push_str(&mathml::to_mathml(&parse_latex(&latex), display));
                }
            }
        }
        if rendered.is_empty() {
            body.push_str("<p><br></p>\n");
        } else {
            body.push_str(&format!("<p>{rendered}</p>\n"));
        }
    }
    format!(
        "<!doctype html>\n<html lang=\"ja\">\n<head>\n<meta charset=\"utf-8\">\n<title>{}</title>\n\
         </head>\n<body>\n{body}</body>\n</html>\n",
        escape_html(title)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_math_is_extracted() {
        assert_eq!(
            parse_line("面積は $\\frac{1}{2}$ です"),
            vec![
                Segment::Text("面積は ".into()),
                Segment::Math {
                    latex: "\\frac{1}{2}".into(),
                    display: false
                },
                Segment::Text(" です".into()),
            ]
        );
    }

    #[test]
    fn display_math_is_recognised() {
        assert_eq!(
            parse_line("$$x^{2}$$"),
            vec![Segment::Math {
                latex: "x^{2}".into(),
                display: true
            }]
        );
    }

    #[test]
    fn escaped_dollar_stays_text() {
        assert_eq!(
            parse_line("100\\$ です"),
            vec![Segment::Text("100$ です".into())]
        );
    }

    #[test]
    fn unmatched_dollar_stays_text() {
        assert_eq!(parse_line("a $ b"), vec![Segment::Text("a $ b".into())]);
    }

    #[test]
    fn escaping_roundtrips() {
        let original = "価格は $100 で \\ を含む";
        let escaped = escape_text(original);
        assert_eq!(parse_line(&escaped), vec![Segment::Text(original.into())]);
    }

    #[test]
    fn blank_lines_are_preserved() {
        assert_eq!(parse("a\n\nb").len(), 3);
    }

    #[test]
    fn html_export_contains_mathml() {
        let html = to_html("$\\frac{1}{2}$", "test");
        assert!(html.contains("<mfrac>"));
        assert!(html.contains("<title>test</title>"));
    }
}
