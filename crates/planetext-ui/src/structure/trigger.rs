//! どの深さのRowでも同じ「文字列 + トリガー文字 + スペース」判定。
//!
//! `/` `^` `_`、矢印や括弧は通常文字のまま入り、スペースで初めて構造へ移る。

use super::ast::{self, Between, Node, NodeKind, Row};
use super::vocabulary;

pub enum Conversion {
    /// 直前の文字を置き換える通常テキスト(`\alpha ` → `α`)。
    Text(String),
    /// 構造を置き、必要なら指定スロットへ入る。
    Structure {
        nodes: Row,
        enter: Option<(usize, usize)>,
    },
}

/// トリガー文字が現在のRowで構造へ移るなら、消費するノード数と置き換え内容。
pub fn conversion_for(row: &[Node], index: usize, c: char) -> Option<(usize, Conversion)> {
    (c == ' ').then(|| trailing(row, index)).flatten()
}

fn trailing(row: &[Node], index: usize) -> Option<(usize, Conversion)> {
    trailing_typed(row, index)
        .or_else(|| trailing_root(row, index))
        .or_else(|| trailing_shortcut(row, index))
}

fn trailing_typed(row: &[Node], index: usize) -> Option<(usize, Conversion)> {
    let trigger = as_char(row.get(index.checked_sub(1)?)?)?;
    if trigger == '/' || ast::is_arrow(trigger) {
        let before = index - 1;
        let (start, taken) = preceding(row, before)?;
        let between = if trigger == '/' {
            Between::Rule
        } else {
            Between::Arrow(trigger)
        };
        let node = Node::stack(taken, Row::new(), between);
        return Some((
            index - start,
            Conversion::Structure {
                nodes: vec![node],
                enter: Some((0, 1)),
            },
        ));
    }
    if !matches!(trigger, '^' | '_') {
        return None;
    }
    preceding(row, index - 1)?;
    // `^` `_` は直前の文字を構造へ移さない。トリガー文字だけを空のスロットに替える。
    let node = if trigger == '^' {
        Node::sup(Row::new())
    } else {
        Node::sub(Row::new())
    };
    let enter = first_entry(&node);
    Some((
        1,
        Conversion::Structure {
            nodes: vec![node],
            enter,
        },
    ))
}

fn preceding(row: &[Node], before: usize) -> Option<(usize, Row)> {
    if let Some(group) = trailing_group(row, before) {
        return Some(group);
    }
    let start = text_start(row, before);
    if start < before {
        return Some((start, row[start..before].to_vec()));
    }
    let start = before.checked_sub(1)?;
    let node = row.get(start)?;
    (!matches!(node.kind, NodeKind::Char(_) | NodeKind::Tab)).then(|| (start, vec![node.clone()]))
}

/// `(x+1)/` の括弧は文字なので、スペースで分数にするときに中身を持ち上げる。
fn trailing_group(row: &[Node], before: usize) -> Option<(usize, Row)> {
    if as_char(row.get(before.checked_sub(1)?)?)? != ')' {
        return None;
    }
    let mut depth = 0;
    for i in (0..before).rev() {
        match as_char(&row[i]) {
            Some(')') => depth += 1,
            Some('(') if depth == 1 => {
                let inner = row[i + 1..before - 1].to_vec();
                return (!inner.is_empty()).then_some((i, inner));
            }
            Some('(') => depth -= 1,
            _ => {}
        }
    }
    None
}

fn text_start(row: &[Node], before: usize) -> usize {
    let mut start = before;
    while start > 0 {
        match as_char(&row[start - 1]) {
            Some(c) if c.is_alphanumeric() || c == '.' => start -= 1,
            _ => break,
        }
    }
    start
}

fn trailing_root(row: &[Node], index: usize) -> Option<(usize, Conversion)> {
    if as_char(row.get(index.checked_sub(1)?)?)? != ']' {
        return None;
    }
    let mut i = index - 1;
    while i > 0 {
        i -= 1;
        if as_char(&row[i]) == Some('[') {
            break;
        }
        if as_char(&row[i]).is_none_or(|c| !c.is_alphanumeric()) {
            return None;
        }
    }
    if as_char(row.get(i)?) != Some('[') {
        return None;
    }
    if as_char(row.get(i.checked_sub(1)?)?)? != '√' {
        return None;
    }
    let index_nodes = row[i + 1..index - 1].to_vec();
    if index_nodes.is_empty() {
        return None;
    }
    let mut node = ast::nth_root();
    if let NodeKind::Sqrt {
        index: Some(slot), ..
    } = &mut node.kind
    {
        *slot = index_nodes;
    }
    Some((
        index - (i - 1),
        Conversion::Structure {
            nodes: vec![node],
            enter: Some((0, 1)),
        },
    ))
}

fn trailing_shortcut(row: &[Node], index: usize) -> Option<(usize, Conversion)> {
    if let Some(start) = command_start(row, index) {
        let name: String = row[start + 1..index].iter().filter_map(as_char).collect();
        if let Some(node) = vocabulary::structure_for(&name) {
            let enter = first_entry(&node);
            return Some((
                index - start,
                Conversion::Structure {
                    nodes: vec![node],
                    enter,
                },
            ));
        }
        if let Some(text) = vocabulary::text_for(&name) {
            return Some((index - start, Conversion::Text(text)));
        }
        return None;
    }
    let glyph = as_char(row.get(index.checked_sub(1)?)?)?;
    let node = vocabulary::node_for_glyph(glyph)?;
    let enter = first_entry(&node);
    Some((
        1,
        Conversion::Structure {
            nodes: vec![node],
            enter,
        },
    ))
}

fn command_start(row: &[Node], index: usize) -> Option<usize> {
    let mut start = index;
    while start > 0 {
        match as_char(&row[start - 1]) {
            Some(c) if c.is_ascii_alphabetic() => start -= 1,
            Some('\\') => return (start < index).then_some(start - 1),
            _ => return None,
        }
    }
    None
}

fn first_entry(node: &Node) -> Option<(usize, usize)> {
    let slot = match &node.kind {
        NodeKind::Stack { .. } => Some(1),
        NodeKind::BigOp(_) => Some(node.lower_slot()),
        _ => node.horizontal_slots().first().copied(),
    }?;
    Some((0, slot))
}

fn as_char(node: &Node) -> Option<char> {
    match node.kind {
        NodeKind::Char(c) => Some(c),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(text: &str) -> Row {
        text.chars().map(Node::char).collect()
    }

    fn kind_of(c: char, text: &str) -> Option<String> {
        let row = chars(text);
        conversion_for(&row, row.len(), c).map(|(_, conversion)| match conversion {
            Conversion::Text(text) => format!("text {text}"),
            Conversion::Structure { nodes, .. } => match &nodes[0].kind {
                NodeKind::Stack { .. } => "stack".into(),
                NodeKind::Sup(_) => "sup".into(),
                NodeKind::Sub(_) => "sub".into(),
                NodeKind::Sqrt { index: Some(_), .. } => "root".into(),
                NodeKind::Sqrt { .. } => "sqrt".into(),
                NodeKind::BigOp(_) => "limits".into(),
                _ => "node".into(),
            },
        })
    }

    #[test]
    fn slash_waits_for_space_after_every_kind_of_text() {
        assert!(kind_of('/', "abc").is_none());
        assert_eq!(kind_of(' ', "abc/"), Some("stack".into()));
        assert_eq!(kind_of(' ', "日本/"), Some("stack".into()));
        assert_eq!(kind_of(' ', "(x+1)/"), Some("stack".into()));
    }

    #[test]
    fn arrows_use_the_same_space_trigger_as_a_fraction() {
        assert!(kind_of('→', "abc").is_none());
        assert_eq!(kind_of(' ', "abc→"), Some("stack".into()));
    }

    #[test]
    fn slash_takes_the_preceding_structure_as_its_upper_row() {
        let root = Node::sqrt(None, "x+1".chars().map(Node::char).collect());
        let row = vec![root.clone(), Node::char('/')];
        let Some((consume, Conversion::Structure { nodes, enter })) =
            conversion_for(&row, row.len(), ' ')
        else {
            panic!("expected a fraction conversion");
        };
        assert_eq!(consume, 2);
        assert_eq!(enter, Some((0, 1)));
        match &nodes[0].kind {
            NodeKind::Stack { above, .. } => assert_eq!(above, &vec![root]),
            other => panic!("expected a fraction, got {other:?}"),
        }
    }

    #[test]
    fn slash_after_punctuation_stays_text() {
        assert!(kind_of(' ', "+/").is_none());
    }

    #[test]
    fn scripts_require_the_same_preceding_item_at_every_depth() {
        assert!(kind_of(' ', "^").is_none());
        assert!(kind_of(' ', "_").is_none());
        assert!(kind_of(' ', "+^").is_none());
        assert_eq!(kind_of(' ', "x^"), Some("sup".into()));
        assert_eq!(kind_of(' ', "日本_"), Some("sub".into()));

        let root = Node::sqrt(None, vec![Node::char('x')]);
        let row = vec![root, Node::char('^')];
        assert!(matches!(
            conversion_for(&row, row.len(), ' '),
            Some((1, Conversion::Structure { .. }))
        ));
    }

    #[test]
    fn commands_only_become_formulas_when_they_need_a_shape() {
        assert_eq!(kind_of(' ', "\\sqrt"), Some("sqrt".into()));
        assert_eq!(kind_of(' ', "\\alpha"), Some("text α".into()));
        assert_eq!(kind_of(' ', "\\sin"), Some("text sin".into()));
        assert!(kind_of(' ', "hello").is_none());
    }

    #[test]
    fn structural_glyphs_wait_for_space_but_plain_symbols_do_not_expand() {
        assert!(kind_of('√', "").is_none());
        assert_eq!(kind_of(' ', "√"), Some("sqrt".into()));
        assert_eq!(kind_of(' ', "√[n]"), Some("root".into()));
        assert!(kind_of(' ', "√[]").is_none());
        assert_eq!(kind_of(' ', "Σ"), Some("limits".into()));
        assert!(kind_of(' ', "α").is_none());
    }
}
