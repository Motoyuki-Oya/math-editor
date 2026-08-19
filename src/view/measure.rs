//! ページを元に戻します。行内の場所がどこで終わったのか、その点は何なのかを示します。
//!
//! 描画では、画面上に物事が表示されます。これは描かれたものを測定します。テキストはブラウザによってプロポーショナル フォントを使用してレイアウトされるため、両方向が必要です。そのため、キャレットがどこに移動するかは行が存在した後でのみわかります。 2 つを離しておくと、間違った場所にあるキャレットは、配置が間違っているか、測定が間違っていることを意味します。両方を一度に行うことはできません。
//!
//! ここでは、何が編集されているかはわかりません。要素と点を取得し、四角形と場所を返します。

use wasm_bindgen::JsCast;
use web_sys::{Element, Range};

use crate::structure::ast::Cursor;
use crate::structure::text::Pos;
use crate::view::row::{self, PATH_ATTR, PLACEHOLDER_CLASS, ROW_CLASS, RUN_CLASS, START_ATTR};

use super::document::LINE_CLASS;

/// ページ独自の座標で画面上の四角形を返します。
#[derive(Clone, Copy, Default)]
pub struct Box2 {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

impl Box2 {
    /// 表示するものがないとして測定された四角形を与えるので、空の行のキャレットはまだ表示されます。
    pub(super) fn fix(mut self) -> Self {
        if self.height <= 0.0 {
            self.height = 18.0;
        }
        if self.width <= 0.0 {
            self.width = 1.0;
        }
        self
    }
}

/// クリックの着地点: テキスト内の場所、またはそこに立っている構造内の場所。
pub enum Hit {
    Text(Pos),
    Inside(Pos, Cursor),
}

pub(super) fn box_of(rect: &web_sys::DomRect) -> Box2 {
    Box2 {
        left: rect.left(),
        top: rect.top(),
        width: rect.width(),
        height: rect.height(),
    }
}

/// 行の 2 つの場所の間にある長方形。行と同じ高さではなく、その範囲にまたがる高さと同じです。分数を選択すると分数全体がカバーされ、単語を選択すると単語全体がカバーされます。
pub(super) fn span_box(row: &Element, left: Box2, right: Box2) -> Box2 {
    let mut top = left.top.min(right.top);
    let mut bottom = (left.top + left.height).max(right.top + right.height);
    let children = row.children();
    for i in 0..children.length() {
        let Some(child) = children.item(i) else {
            continue;
        };
        let rect = box_of(&child.get_bounding_client_rect());
        let middle = rect.left + rect.width / 2.0;
        if middle > left.left && middle < right.left {
            top = top.min(rect.top);
            bottom = bottom.max(rect.top + rect.height);
        }
    }
    Box2 {
        left: left.left,
        top,
        width: (right.left - left.left).max(1.0),
        height: bottom - top,
    }
    .fix()
}

/// 長方形は、行のストレッチを一度に 1 つずつカバーします。行が 1 行にある場合は 1 つ、折り返される場合は繰り上げ行ごとに 1 つです。 `to` は行の終わりの `usize::MAX` にすることができ、`past_end` は最後の部分を広げて、行を越えた選択範囲で改行がギャップとして表示されるようにします。
pub(super) fn span_boxes(row: &Element, from: usize, to: usize, past_end: bool) -> Vec<Box2> {
    let mut pieces: Vec<Vec<Box2>> = Vec::new();
    let mut piece: Vec<Box2> = Vec::new();
    for (index, rect) in boundaries(row) {
        if index < from || (to != usize::MAX && index > to) {
            continue;
        }
        // 別の行に落ちた場所から新しい部分が始まります。ブラウザが行を転送した場所です。
        if piece
            .last()
            .is_some_and(|last| (last.top - rect.top).abs() > 1.0)
        {
            pieces.push(std::mem::take(&mut piece));
        }
        piece.push(rect);
    }
    if !piece.is_empty() {
        pieces.push(piece);
    }
    let last = pieces.len();
    pieces
        .into_iter()
        .enumerate()
        .filter_map(|(nth, piece)| {
            let left = *piece.first()?;
            let mut right = *piece.last()?;
            if past_end && nth + 1 == last {
                right.left += 6.0;
            }
            Some(span_box(row, left, right))
        })
        .collect()
}

/// 任意の深さで点が最も近い行内の場所。点が入っている最も内側の行が決定するため、分母をクリックすると着地します。
pub(super) fn hit_in_line(holder: &Element, line: usize, x: f64, y: f64) -> Hit {
    let Some(row) = innermost_row(holder, x, y) else {
        return Hit::Text(Pos::new(line, 0));
    };
    let index = nearest_index(&row, x, y);
    let path = row
        .get_attribute(PATH_ATTR)
        .and_then(|encoded| row::decode_path(&encoded))
        .unwrap_or_default();
    match path.first() {
        None => Hit::Text(Pos::new(line, index)),
        Some((col, _)) => Hit::Inside(
            Pos::new(line, *col),
            Cursor {
                path,
                index,
                anchor: index,
                fills: Vec::new(),
            },
        ),
    }
}

/// ポイントが内側にある最も深い行。行は入れ子になっているため、分母内の点は分数の行内と直線の行内にもあります。
fn innermost_row(holder: &Element, x: f64, y: f64) -> Option<Element> {
    let rows = holder.query_selector_all(&format!(".{ROW_CLASS}")).ok()?;
    let mut best: Option<(usize, Element)> = None;
    let mut fallback: Option<Element> = None;
    for i in 0..rows.length() {
        let Some(row) = rows
            .item(i)
            .and_then(|node| node.dyn_ref::<Element>().cloned())
        else {
            continue;
        };
        let depth = row
            .get_attribute(PATH_ATTR)
            .and_then(|encoded| row::decode_path(&encoded))
            .map(|path| path.len())
            .unwrap_or(0);
        if depth == 0 {
            fallback = Some(row.clone());
        }
        let rect = row.get_bounding_client_rect();
        let inside = y >= rect.top() && y <= rect.bottom() && x >= rect.left() && x <= rect.right();
        if inside && best.as_ref().is_none_or(|(deepest, _)| depth > *deepest) {
            best = Some((depth, row));
        }
    }
    best.map(|(_, row)| row).or(fallback)
}

/// 点に最も近い行内の場所。ポイントが置かれている行は、その行に沿った場所より前にカウントされるため、折り返された行の 2 行目をクリックすると、最初の行と同じ列ではなく、そこに移動します。
fn nearest_index(row: &Element, x: f64, y: f64) -> usize {
    let mut best = (f64::MAX, 0usize);
    for (index, rect) in boundaries(row) {
        let above = y - (rect.top + rect.height);
        let below = rect.top - y;
        let away = above.max(below).max(0.0);
        let distance = away * 1000.0 + (rect.left - x).abs();
        if distance < best.0 {
            best = (distance, index);
        }
    }
    best.1
}

/// 行内のすべての場所と、それが画面上のどこにあるか。
fn boundaries(row: &Element) -> Vec<(usize, Box2)> {
    let mut places = Vec::new();
    let children = row.children();
    for i in 0..children.length() {
        let Some(child) = children.item(i) else {
            continue;
        };
        let Some(start) = start_of(&child) else {
            continue;
        };
        match run_length(&child) {
            Some(len) => {
                for offset in 0..=len {
                    if let Some(rect) = text_boundary(&child, offset) {
                        places.push((start + offset, rect));
                    }
                }
            }
            None => {
                let rect = box_of(&child.get_bounding_client_rect());
                // 空の行には、クリックするボックスがありますが、その後に場所はありません。
                if child.class_list().contains(PLACEHOLDER_CLASS) {
                    places.push((start, rect));
                    continue;
                }
                places.push((start, rect));
                places.push((
                    start + 1,
                    Box2 {
                        left: rect.left + rect.width,
                        ..rect
                    },
                ));
            }
        }
    }
    places
}

/// `index` の項目の直前の場所。 `usize::MAX` は終了を意味します。
pub(super) fn first_base_fragment(holder: &Element) -> Option<Box2> {
    let nodes = holder.query_selector_all(&format!("[{START_ATTR}]")).ok()?;
    let mut best: Option<Box2> = None;
    for i in 0..nodes.length() {
        let Some(element) = nodes
            .item(i)
            .and_then(|node| node.dyn_ref::<Element>().cloned())
        else {
            continue;
        };
        if element.children().length() > 0 || element.closest(".mn-limit").ok().flatten().is_some()
        {
            continue;
        }
        let Some(rect) = (if element.class_list().contains(RUN_CLASS) {
            text_boundary(&element, 0)
        } else {
            Some(box_of(&element.get_bounding_client_rect()))
        }) else {
            continue;
        };
        if rect.height <= 0.0 {
            continue;
        }
        if best.is_none_or(|current| {
            rect.top < current.top || (rect.top == current.top && rect.left < current.left)
        }) {
            best = Some(rect);
        }
    }
    best
}

pub(super) fn boundary(row: &Element, index: usize) -> Option<Box2> {
    let places = boundaries(row);
    if index == usize::MAX {
        return places.last().map(|(_, rect)| *rect);
    }
    places
        .iter()
        .find(|(place, _)| *place == index)
        .map(|(_, rect)| *rect)
        .or_else(|| places.last().map(|(_, rect)| *rect))
}

/// テキスト ランが保持する文字数、または構造など、独自の 1 つの場所を占めるものについては `None` を表します。
fn run_length(child: &Element) -> Option<usize> {
    child
        .class_list()
        .contains(RUN_CLASS)
        .then(|| child.text_content().unwrap_or_default().chars().count())
}

pub(super) fn start_of(child: &Element) -> Option<usize> {
    child.get_attribute(START_ATTR)?.parse().ok()
}

/// 折りたたまれた範囲でテキスト ラン内の場所を測定します。これが、プロポーショナル フォントのオフセットの位置を取得する唯一の方法です。
fn text_boundary(run: &Element, offset: usize) -> Option<Box2> {
    let doc = run.owner_document()?;
    let text = run.text_content().unwrap_or_default();
    let Some(node) = run.first_child() else {
        // 空の行には、測定するテキスト ノードがありません。
        return Some(empty_run_box(run));
    };
    let units: u32 = text
        .chars()
        .take(offset)
        .map(|c| c.len_utf16() as u32)
        .sum();
    let range: Range = doc.create_range().ok()?;
    range.set_start(&node, units).ok()?;
    range.set_end(&node, units).ok()?;
    let rect = box_of(&range.get_bounding_client_rect());
    if rect.height > 0.0 {
        return Some(rect);
    }
    // 空のテキスト ノード内の折りたたまれた範囲には、何もありません。ボックス;実行自体を使用します。
    Some(empty_run_box(run))
}

/// 空のインライン実行にはそれ自体の高さがなく、キャレットが非表示のままになるため、高さは実行が配置されている行から取得されます。
fn empty_run_box(run: &Element) -> Box2 {
    let rect = box_of(&run.get_bounding_client_rect());
    if rect.height > 0.0 {
        return rect;
    }
    let Some(line) = run.closest(&format!(".{LINE_CLASS}")).ok().flatten() else {
        return rect;
    };
    let holder = box_of(&line.get_bounding_client_rect());
    Box2 {
        left: rect.left,
        top: holder.top,
        width: rect.width,
        height: holder.height,
    }
}
