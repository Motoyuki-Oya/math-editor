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

/// すごいクリックです。着地: テキスト内の場所、またはそこに立っている島内の場所。
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
    let min_x = left.left.min(right.left);
    let max_x = left.left.max(right.left);
    let mut top = left.top.min(right.top);
    let mut bottom = (left.top + left.height).max(right.top + right.height);
    let children = row.children();
    for i in 0..children.length() {
        let Some(child) = children.item(i) else {
            continue;
        };
        let rect = box_of(&child.get_bounding_client_rect());
        let middle = rect.left + rect.width / 2.0;
        if middle >= min_x && middle <= max_x {
            top = top.min(rect.top);
            bottom = bottom.max(rect.top + rect.height);
        }
    }
    Box2 {
        left: min_x,
        top,
        width: (max_x - min_x).max(1.0),
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
            if piece.is_empty() {
                return None;
            }
            let first = *piece.first()?;
            let last_box = *piece.last()?;
            let mut res = span_box(row, first, last_box);
            let min_left = piece.iter().map(|r| r.left).fold(f64::INFINITY, f64::min);
            let mut max_right = piece.iter().map(|r| r.left).fold(f64::NEG_INFINITY, f64::max);
            if past_end && nth + 1 == last {
                max_right += 6.0;
            }
            res.left = min_left;
            res.width = (max_right - min_left).max(1.0);
            Some(res)
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
                let text = child.text_content().unwrap_or_default();
                use unicode_segmentation::UnicodeSegmentation;
                let mut char_offset = 0;
                for grapheme in text.graphemes(true) {
                    if let Some(rect) = text_boundary(&child, char_offset) {
                        places.push((start + char_offset, rect));
                    }
                    char_offset += grapheme.chars().count();
                }
                if let Some(rect) = text_boundary(&child, len) {
                    places.push((start + len, rect));
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

pub(super) fn visual_neighbor(row: &Element, index: usize, right: bool) -> Option<usize> {
    visual_neighbor_in(boundaries(row), index, right)
}

fn visual_neighbor_in(mut places: Vec<(usize, Box2)>, index: usize, right: bool) -> Option<usize> {
    let mut seen = std::collections::HashSet::new();
    places.retain(|(index, _)| seen.insert(*index));
    let line_height = places
        .iter()
        .map(|(_, r)| r.height)
        .fold(0.0, f64::max)
        .max(8.0);
    let bucket_size = (line_height * 0.5).max(4.0);
    places.sort_by(|(_, a), (_, b)| {
        let bucket_a = (a.top / bucket_size).round() as i64;
        let bucket_b = (b.top / bucket_size).round() as i64;
        bucket_a
            .cmp(&bucket_b)
            .then_with(|| a.left.total_cmp(&b.left))
            .then_with(|| a.top.total_cmp(&b.top))
    });
    let current = places
        .iter()
        .position(|(candidate, _)| *candidate == index)?;
    let next = if right {
        current.checked_add(1)?
    } else {
        current.checked_sub(1)?
    };
    places.get(next).map(|(index, _)| *index)
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

/// テキスト ランが保持する文字数、または島や構造物など、独自の 1 つの場所を占めるものについては `None` を表します。
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

#[cfg(test)]
mod tests {
    use super::*;

    fn at(index: usize, left: f64) -> (usize, Box2) {
        (
            index,
            Box2 {
                left,
                top: 0.0,
                width: 1.0,
                height: 10.0,
            },
        )
    }

    #[test]
    fn visual_neighbors_follow_mixed_bidi_positions() {
        let places = vec![
            at(0, 100.0),
            at(1, 90.0),
            at(2, 80.0),
            at(3, 40.0),
            at(4, 50.0),
            at(5, 60.0),
            at(6, 70.0),
        ];
        assert_eq!(visual_neighbor_in(places.clone(), 0, false), Some(1));
        assert_eq!(visual_neighbor_in(places.clone(), 0, true), None);
        assert_eq!(visual_neighbor_in(places.clone(), 4, true), Some(5));
        assert_eq!(visual_neighbor_in(places, 4, false), Some(3));
    }
}
