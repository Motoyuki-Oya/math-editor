//! コピーによってクリップボードに保存される内容、およびそこにあるものからの貼り付けによって何が作成されるか。
//!
//! クリップボードはファイルではないため、ここではファイル形式を介したものは何もありません。エディターから出力されるのは通常のテキストであり、文書が画面上で読み取られる方法を読み取ります。部分自体は、出力されたテキストの横にここに保持されるため、エディタに戻ってくるコピーはその形状を維持しますが、他の場所からのテキストはそのままの文字として到着します。

use std::cell::RefCell;

use crate::structure::ast::{Node, Row};
use crate::structure::plain;
use crate::structure::text::Item;

/// クリップボードを通過する途中のドキュメントの一部。
#[derive(Clone)]
pub enum Clip {
    /// テキストの行、構造が含まれる。
    Text(Vec<Vec<Item>>),
    /// 構造の 1 行の一部。
    Row(Row),
}

impl Clip {
    /// 部分が通常のテキストとして読み取られる方法。
    pub fn text(&self) -> String {
        match self {
            Clip::Text(lines) => plain::lines(lines),
            Clip::Row(row) => plain::row(row),
        }
    }

    /// テキストの行としての部分: プレーン文字の行はそれらの文字であり、形状を持つ行が 1 つの構造になります。
    pub fn items(&self) -> Vec<Vec<Item>> {
        match self {
            Clip::Text(lines) => lines.clone(),
            Clip::Row(row) => {
                let chars: Option<Vec<Item>> = row
                    .iter()
                    .map(|node| match node {
                        Node::Char(c) => Some(Item::Char(*c)),
                        _ => None,
                    })
                    .collect();
                match chars {
                    Some(items) => vec![items],
                    None => vec![vec![Item::Math(row.clone())]],
                }
            }
        }
    }

    /// 構造内に貼り付けるための、行の一部としての部分です。
    pub fn row(&self) -> Row {
        match self {
            Clip::Row(row) => row.clone(),
            Clip::Text(lines) => lines
                .iter()
                .flatten()
                .flat_map(|item| match item {
                    Item::Char(c) => vec![Node::Char(*c)],
                    // 構造内の構造は、行内にある独自のものです。列区切り文字は、一方では意味を持ちません。
                    Item::Math(row) => row.clone(),
                    Item::Tab => Vec::new(),
                })
                .collect(),
        }
    }
}

thread_local! {
    /// コピーされた最後の部分と、その部分に配布されたテキストです。
    static KEPT: RefCell<Option<(String, Clip)>> = const { RefCell::new(None) };
}

/// コピーされた部分を保持し、クリップボードに置くテキストを返します。
pub fn keep(clip: Clip) -> String {
    let text = clip.text();
    KEPT.with(|kept| *kept.borrow_mut() = Some((text.clone(), clip)));
    text
}

/// 貼り付けられたテキストがそのまま出力された場合、コピーされた部分。それ以外のものは外部のどこかから来たものであり、プレーン テキストです。
pub fn pasted(text: &str) -> Option<Clip> {
    KEPT.with(|kept| {
        kept.borrow()
            .as_ref()
            .filter(|(handed, _)| handed == text)
            .map(|(_, clip)| clip.clone())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::structure::ast::Between;

    fn fraction() -> Row {
        vec![Node::Stack {
            above: vec![Node::Char('a')],
            below: vec![Node::Char('b')],
            between: Between::Rule,
        }]
    }

    #[test]
    fn a_copied_structure_leaves_as_plain_text_and_comes_back_as_itself() {
        let text = keep(Clip::Text(vec![vec![Item::Math(fraction())]]));
        assert_eq!(text, "a/b");
        let clip = pasted(&text).expect("the piece that was copied");
        assert_eq!(clip.items(), vec![vec![Item::Math(fraction())]]);
    }

    #[test]
    fn text_from_elsewhere_is_not_mistaken_for_the_copied_piece() {
        keep(Clip::Text(vec![vec![Item::Math(fraction())]]));
        assert!(pasted("a/b ").is_none());
        assert!(pasted("x/y").is_none());
    }

    /// 空の構造は何も読み取られないため、コピーするものがあるかどうかはテキストによって決定できません。それが選択の仕事です。
    #[test]
    fn an_empty_structure_reads_as_nothing() {
        assert_eq!(Clip::Text(vec![vec![Item::Math(Vec::new())]]).text(), "");
    }

    /// 構造からコピーされたプレーン キャラクタはプレーン キャラクタです。
    #[test]
    fn a_row_of_characters_pastes_as_characters() {
        let clip = Clip::Row(vec![Node::Char('a'), Node::Char('b')]);
        assert_eq!(clip.text(), "ab");
        assert_eq!(clip.items(), vec![vec![Item::Char('a'), Item::Char('b')]]);
    }
}
