//! Reads and writes a complete document. `$()` belongs only to this file-format layer.

use super::islands::{self, Segment};
use super::notation::{island_text, parse_island};
use crate::structure::ast::{Node, NodeKind, Row};
use crate::structure::text::{SourceLine, Text};

pub const TAB_SOURCE: &str = "t";

pub fn read(source: &str) -> Text {
    Text::compose(source.split('\n').map(read_line).collect())
}

/// ファイルの 1 行を読み取ります。範囲読みの読み込みが、届いた行をここで
/// 変換します。ファイル以外（クリップボードなど）がここを通ってはいけません。
pub fn read_line(line: &str) -> SourceLine {
    // `$` を含まない行には島もエスケープもないので、文字列のまま持ち、
    // そのまま書き戻せる。巨大なファイルの大部分がこの形で済む。
    if !line.contains('$') {
        return SourceLine::Plain(line.to_string());
    }
    let row = islands::parse_line(line)
        .into_iter()
        .flat_map(|segment| match segment {
            Segment::Text(text) => text.chars().map(Node::char).collect::<Row>(),
            Segment::Island(source) if source.trim() == TAB_SOURCE => vec![Node::tab()],
            Segment::Island(source) => parse_island(&source),
        })
        .collect();
    SourceLine::Parsed(row)
}

/// 文書全体をファイルの形にします。書き込み自体は文書の本体（ネイティブ側）が
/// 行うようになったため製品コードからは呼ばれないが、読み書きが往復することの
/// 検査と、この層が形式の定義であることのために残る。
#[allow(dead_code)]
pub fn write(text: &Text) -> String {
    (0..text.line_count())
        .map(|line| match text.raw_line(line) {
            // 素のままの行は `$` を含まないので、エスケープなしで元の文字列のまま。
            Some(source) => source.to_string(),
            None => write_line(text.line(line)),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// ファイルの 1 行を書き出します。編集された行を文書の本体へ届けるときに使います。
pub fn write_line(row: &[Node]) -> String {
    islands::serialize_line(&line_segments(row))
}

fn line_segments(row: &[Node]) -> islands::Line {
    let mut segments = islands::Line::new();
    let mut text = String::new();
    for node in row {
        let source = match &node.kind {
            NodeKind::Char(c) if node.upper.is_empty() && node.lower.is_empty() => {
                text.push(*c);
                continue;
            }
            NodeKind::Tab if node.upper.is_empty() && node.lower.is_empty() => {
                TAB_SOURCE.to_string()
            }
            _ => island_text(&vec![node.clone()]),
        };
        if !text.is_empty() {
            segments.push(Segment::Text(std::mem::take(&mut text)));
        }
        segments.push(Segment::Island(source));
    }
    if !text.is_empty() {
        segments.push(Segment::Text(text));
    }
    segments
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::structure::ast::NodeKind;
    use crate::structure::text::Pos;

    #[test]
    fn structures_are_sibling_nodes_in_the_document_row() {
        let text = read("a + $(b/c) + d");
        assert_eq!(text.line_len(0), 9);
        assert!(matches!(
            text.node_at(Pos::new(0, 4)).map(|n| &n.kind),
            Some(NodeKind::Stack { .. })
        ));
        assert!(matches!(
            text.node_at(Pos::new(0, 5)).map(|n| &n.kind),
            Some(NodeKind::Char(' '))
        ));
    }

    #[test]
    fn a_column_separator_is_one_node() {
        let text = read("a $(t) b");
        assert_eq!(text.line_len(0), 5);
        assert!(matches!(
            text.node_at(Pos::new(0, 2)).map(|n| &n.kind),
            Some(NodeKind::Tab)
        ));
    }

    #[test]
    fn documents_survive_a_load_and_save() {
        for source in [
            "a $(1/2) b",
            "a $(t) b",
            "$(√[3] x)",
            "x$(^ 3)$(_ i)",
            "$(↨ Σ, n, x=1)",
            "$([a, b][c, d])",
            "$(a → b)",
            "$(√ x$(^ 3))",
            "100$$ です",
        ] {
            assert_eq!(write(&read(source)), source);
        }
    }

    #[test]
    fn plain_lines_stay_raw_and_are_written_verbatim() {
        let source = "abc\nx $(1/2) y\n100$$ です";
        let text = read(source);
        assert_eq!(text.raw_line(0), Some("abc"));
        // `$` を含む行は解析され、書くときにエスケープを通る。
        assert_eq!(text.raw_line(1), None);
        assert_eq!(text.raw_line(2), None);
        assert_eq!(write(&text), source);
    }

    /// 一時的な規模チェック: `cargo test --release -- --ignored` で実行する。
    #[test]
    #[ignore]
    fn scale_check_hundred_megabytes() {
        let line = "The quick brown fox jumps over the lazy dog. 0123";
        let count = 2_000_000usize; // 約100MB
        let mut source = String::with_capacity(51 * count);
        for _ in 0..count {
            source.push_str(line);
            source.push('\n');
        }
        let start = std::time::Instant::now();
        let text = read(&source);
        println!("read: {:?}", start.elapsed());
        let start = std::time::Instant::now();
        let stats = text.stats();
        println!("stats: {:?} {stats:?}", start.elapsed());
        // 可視範囲相当の行アクセス。
        let start = std::time::Instant::now();
        for line in 1_000_000..1_000_100 {
            let _ = text.line(line);
        }
        println!("view 100 lines: {:?}", start.elapsed());
        let start = std::time::Instant::now();
        let written = write(&text);
        println!("write: {:?}", start.elapsed());
        assert_eq!(written.len(), source.len());
    }

    #[test]
    fn grouped_plain_characters_may_save_canonically() {
        let text = read("a + $(b/c) + d");
        assert_eq!(write(&text), "a + $(b/c) + d");
        assert_eq!(read(&write(&text)), text);
    }
}
