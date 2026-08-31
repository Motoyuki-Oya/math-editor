//! ファイル形式: 通常のテキスト。2 次元レイアウトが必要な部分はアイランド `$( ... )` として記述されます。普通のドル記号は `$$` と書かれます。

use super::notation;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Segment {
    Text(String),
    /// 島の内容。周囲の `$(` と `)` はありません。
    Island(String),
}

pub type Line = Vec<Segment>;

/// 戻って読むと同じ文字が得られるようにテキストを書き込みます。
pub fn escape_text(text: &str) -> String {
    text.replace('$', "$$")
}

pub fn parse_line(line: &str) -> Line {
    let chars: Vec<char> = line.chars().collect();
    let mut segments = Line::new();
    let mut text = String::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '$' if chars.get(i + 1) == Some(&'$') => {
                text.push('$');
                i += 2;
            }
            '$' if chars.get(i + 1) == Some(&'(') => match notation::island_end(&chars, i) {
                Some(end) => {
                    if !text.is_empty() {
                        segments.push(Segment::Text(std::mem::take(&mut text)));
                    }
                    segments.push(Segment::Island(chars[i + 2..end].iter().collect()));
                    i = end + 1;
                }
                // 決して閉じられない島はテキストだけです。
                None => {
                    text.push('$');
                    i += 1;
                }
            },
            c => {
                text.push(c);
                i += 1;
            }
        }
    }
    if !text.is_empty() {
        segments.push(Segment::Text(text));
    }
    segments
}

/// `parse_line` が読み取った形式で 1 行を書き戻します。
pub fn serialize_line(segments: &Line) -> String {
    segments
        .iter()
        .map(|segment| match segment {
            Segment::Text(text) => escape_text(text),
            Segment::Island(source) => format!("$({source})"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_island_is_taken_out_of_the_text() {
        assert_eq!(
            parse_line("面積は $(1/2)ah です"),
            vec![
                Segment::Text("面積は ".into()),
                Segment::Island("1/2".into()),
                Segment::Text("ah です".into()),
            ]
        );
    }

    #[test]
    fn islands_may_contain_islands() {
        assert_eq!(
            parse_line("$(√ x$(^ 3 _ i))"),
            vec![Segment::Island("√ x$(^ 3 _ i)".into())]
        );
    }

    #[test]
    fn brackets_and_slashes_in_the_text_stay_text() {
        let line = "[a, b; c, d] と {} と √ と 1/2";
        assert_eq!(parse_line(line), vec![Segment::Text(line.into())]);
    }

    #[test]
    fn a_doubled_dollar_is_one_dollar() {
        assert_eq!(
            parse_line("100$$ です"),
            vec![Segment::Text("100$ です".into())]
        );
    }

    #[test]
    fn a_parenthesis_without_a_dollar_is_text() {
        assert_eq!(parse_line("(a)"), vec![Segment::Text("(a)".into())]);
    }

    #[test]
    fn an_unclosed_island_stays_text() {
        assert_eq!(parse_line("a $(b"), vec![Segment::Text("a $(b".into())]);
    }

    #[test]
    fn escaping_roundtrips() {
        let original = "価格は $100、$( も素のまま";
        assert_eq!(
            parse_line(&escape_text(original)),
            vec![Segment::Text(original.into())]
        );
    }

    #[test]
    fn blank_lines_are_preserved() {
        assert_eq!("a\n\nb".split('\n').map(parse_line).count(), 3);
    }
}
