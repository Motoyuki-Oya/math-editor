//! Reads the page back: where a place in a row ended up, and what a point is
//! on.
//!
//! Drawing puts things on screen; this measures what was drawn. Both directions
//! are needed because the text is laid out by the browser with a proportional
//! font, so where a caret goes is only known after the row exists. Keeping the
//! two apart means a caret in the wrong place is either put wrong or measured
//! wrong, never both at once.
//!
//! Nothing here knows what is being edited — it takes elements and points and
//! gives back rectangles and places.

use wasm_bindgen::JsCast;
use web_sys::{Element, Range};

use crate::structure::ast::Cursor;
use crate::structure::text::Pos;
use crate::view::row::{self, PATH_ATTR, PLACEHOLDER_CLASS, ROW_CLASS, RUN_CLASS, START_ATTR};

use super::document::LINE_CLASS;

/// A rectangle on screen, in the page's own coordinates.
#[derive(Clone, Copy, Default)]
pub struct Box2 {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

impl Box2 {
    /// Gives a rectangle that measured as nothing something to show, so a caret
    /// on an empty row is still visible.
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

/// What a click landed on: a place in the text, or a place inside the island
/// standing there.
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

/// The rectangle between two places of a row, as tall as what it spans rather
/// than as tall as a line: selecting a fraction covers the whole fraction, the
/// way selecting a word covers the whole word.
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

/// The place in a row a point is nearest to, at whatever depth: the innermost
/// row the point is in decides, so clicking a denominator lands in the
/// denominator and not beside the fraction.
pub(super) fn hit_in_line(holder: &Element, line: usize, x: f64, y: f64) -> Hit {
    let Some(row) = innermost_row(holder, x, y) else {
        return Hit::Text(Pos::new(line, 0));
    };
    let index = nearest_index(&row, x);
    let path = row
        .get_attribute(PATH_ATTR)
        .and_then(|encoded| row::decode_path(&encoded))
        .unwrap_or_default();
    match path.split_first() {
        None => Hit::Text(Pos::new(line, index)),
        Some(((col, _), rest)) => Hit::Inside(
            Pos::new(line, *col),
            Cursor {
                path: rest.to_vec(),
                index,
                anchor: index,
                fills: Vec::new(),
            },
        ),
    }
}

/// The deepest row the point is inside of. Rows nest, so a point inside a
/// denominator is inside the fraction's row and the line's row as well.
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

/// The place in a row nearest to a point.
fn nearest_index(row: &Element, x: f64) -> usize {
    let mut best = (f64::MAX, 0usize);
    for (index, rect) in boundaries(row) {
        let distance = (rect.left - x).abs();
        if distance < best.0 {
            best = (distance, index);
        }
    }
    best.1
}

/// Every place in a row, and where it is on screen.
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
                // An empty row has a box to click on but no place after it.
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

/// The place just before the item at `index`; `usize::MAX` means the end.
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

/// How many characters a text run holds, or `None` for anything that takes one
/// place of its own, such as an island or a structure.
fn run_length(child: &Element) -> Option<usize> {
    child
        .class_list()
        .contains(RUN_CLASS)
        .then(|| child.text_content().unwrap_or_default().chars().count())
}

pub(super) fn start_of(child: &Element) -> Option<usize> {
    child.get_attribute(START_ATTR)?.parse().ok()
}

/// Measures a place inside a text run with a collapsed range, which is the only
/// way to get an offset's position for a proportional font.
fn text_boundary(run: &Element, offset: usize) -> Option<Box2> {
    let doc = run.owner_document()?;
    let text = run.text_content().unwrap_or_default();
    let Some(node) = run.first_child() else {
        // An empty line has no text node to measure.
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
    // A collapsed range in an empty text node has no box; use the run itself.
    Some(empty_run_box(run))
}

/// An empty inline run has no height of its own, which would leave the caret
/// invisible, so the height comes from the line the run sits on.
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
