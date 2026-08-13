//! Draws the document, the carets and the selections into the page.
//!
//! Every line is drawn by [`crate::view::row`], the one component that draws a
//! row, so a line of text and the inside of a structure are the same thing to
//! everything here. The only thing a line does that a row inside a structure
//! cannot is line its column separators up with its neighbours.
//!
//! Nothing here is editable by the browser: lines are plain spans, and every
//! caret is a small absolutely placed element, which is how several carets can
//! be shown at once, and how a caret inside a structure is the same caret as a
//! caret in the text.

use wasm_bindgen::JsCast;
use web_sys::{Document, Element, HtmlElement, Range};

use crate::structure::ast::Cursor;
use crate::structure::text::{Item, Pos, Sel, Text};
use crate::view::row::{
    self, Path, Preedit, Renderer, FIELD_CLASS, PATH_ATTR, PLACEHOLDER_CLASS, ROW_CLASS, RUN_CLASS,
    START_ATTR, TAB_CLASS,
};

pub const LINE_CLASS: &str = "mn-line";
/// The gap left after the widest cell of a column.
const COLUMN_GAP: f64 = 18.0;
const LINE_ATTR: &str = "data-line";

pub struct View {
    /// Scrolls, and receives the mouse.
    pub root: HtmlElement,
    lines: Element,
    overlay: Element,
}

/// Where the caret is, which is all the drawing needs to know about it: a place
/// in the text, how deep into the structure there it reaches, and what an IME is
/// composing at it. There is no mode: the same caret describes both cases.
#[derive(Default)]
pub struct Caret<'a> {
    pub at: Pos,
    /// Set when the caret is inside the structure at `at`.
    pub inside: Option<&'a Cursor>,
    /// Text an IME has not committed yet, drawn where it will land.
    pub composing: Option<&'a str>,
}

impl Caret<'_> {
    /// The row the caret is in, and how far into it it is. A caret in the text
    /// is in the row of the line; a caret inside a structure is in a row of
    /// that structure, reached through the island standing at `at`.
    fn place(&self) -> (Vec<(usize, usize)>, usize) {
        match self.inside {
            None => (Vec::new(), self.at.col),
            Some(cursor) => {
                let mut path = vec![(self.at.col, 0)];
                path.extend_from_slice(&cursor.path);
                (path, cursor.index)
            }
        }
    }
}

/// What a click landed on: a place in the text, or a place inside the island
/// standing there.
pub enum Hit {
    Text(Pos),
    Inside(Pos, Cursor),
}

impl View {
    /// Builds the layers inside `root`, which the caller owns.
    pub fn new(root: HtmlElement) -> Option<Self> {
        let doc = root.owner_document()?;
        root.set_inner_html("");
        let content = element(&doc, "div", "mn-content")?;
        let overlay = element(&doc, "div", "mn-overlay")?;
        let lines = element(&doc, "div", "mn-lines")?;
        append(&content, &overlay);
        append(&content, &lines);
        append(&root, &content);
        Some(Self {
            root,
            lines,
            overlay,
        })
    }

    pub fn draw(&self, text: &Text, sels: &[Sel], caret: &Caret<'_>, focused: bool) {
        let Some(doc) = self.lines.owner_document() else {
            return;
        };
        // Text an IME is composing belongs at the caret, whichever row that is
        // in: the component that draws the row puts it there.
        let (path, index) = caret.place();
        let preedit = caret.composing.map(|text| Preedit { path, index, text });
        self.lines.set_inner_html("");
        for line in 0..text.line_count() {
            // Only the line the caret is on shows what is being composed.
            let here = preedit
                .as_ref()
                .filter(|_| caret.at.line == line)
                .map(|preedit| Preedit {
                    path: preedit.path.clone(),
                    index: preedit.index,
                    text: preedit.text,
                });
            if let Some(element) = self.draw_line(&doc, text, line, here.as_ref()) {
                append(&self.lines, &element);
            }
        }
        self.align_columns(text);
        self.draw_carets(&doc, sels, caret, focused);
    }

    fn draw_line(
        &self,
        doc: &Document,
        text: &Text,
        line: usize,
        preedit: Option<&Preedit<'_>>,
    ) -> Option<Element> {
        let holder = element(doc, "div", LINE_CLASS)?;
        holder.set_attribute(LINE_ATTR, &line.to_string()).ok();
        let renderer = Renderer::new(doc).with_preedit(preedit);
        append(&holder, &renderer.line(text.line(line)));
        Some(holder)
    }

    /// Lines up the column separators of neighbouring lines. This is the one
    /// thing a line does that a row inside a structure does not: it is about
    /// several lines at once, which only the document has.
    fn align_columns(&self, text: &Text) {
        let mut line = 0;
        while line < text.line_count() {
            if !has_tab(text.line(line)) {
                line += 1;
                continue;
            }
            let mut end = line;
            while end < text.line_count() && has_tab(text.line(end)) {
                end += 1;
            }
            self.align_block(line..end);
            line = end;
        }
    }

    fn align_block(&self, block: std::ops::Range<usize>) {
        let tabs: Vec<Vec<Element>> = block
            .map(|line| match self.line_row(line) {
                Some(row) => children_of_class(&row, TAB_CLASS),
                None => Vec::new(),
            })
            .collect();
        let columns = tabs.iter().map(Vec::len).max().unwrap_or(0);
        for column in 0..columns {
            // One column at a time, because widening a column moves the ones
            // after it, and the measurements have to follow.
            let separators: Vec<&Element> =
                tabs.iter().filter_map(|line| line.get(column)).collect();
            let widest = separators
                .iter()
                .map(|tab| tab.get_bounding_client_rect().left())
                .fold(f64::MIN, f64::max);
            for tab in separators {
                let left = tab.get_bounding_client_rect().left();
                let width = (widest - left + COLUMN_GAP).max(1.0);
                tab.set_attribute("style", &format!("width:{width}px")).ok();
            }
        }
    }

    /// Draws every caret and every selection, in the text and inside a
    /// structure alike: they are all rectangles measured from what was drawn.
    fn draw_carets(&self, doc: &Document, sels: &[Sel], caret: &Caret<'_>, focused: bool) {
        self.overlay.set_inner_html("");
        let origin = self.lines.get_bounding_client_rect();
        // While an IME is composing, the underlined text stands in for the
        // caret, wherever it is.
        let show_carets = focused && caret.composing.is_none();
        if let Some(cursor) = caret.inside {
            if !cursor.is_caret() {
                let (path, _) = caret.place();
                if let Some(rect) =
                    self.span_in_row(caret.at.line, &path, cursor.start(), Some(cursor.end()))
                {
                    self.shade(doc, rect, &origin);
                }
            }
            if show_carets {
                let (path, index) = caret.place();
                if let Some(rect) = self.place_box(caret.at.line, &path, index) {
                    self.mark_caret(doc, rect, &origin, true);
                }
            }
            return;
        }
        for (index, sel) in sels.iter().enumerate() {
            if !sel.is_caret() {
                for rect in self.selection_rects(*sel) {
                    self.shade(doc, rect, &origin);
                }
            }
            if !show_carets {
                continue;
            }
            if let Some(rect) = self.caret_rect(sel.head) {
                self.mark_caret(doc, rect, &origin, index + 1 == sels.len());
            }
        }
    }

    fn shade(&self, doc: &Document, rect: Box2, origin: &web_sys::DomRect) {
        if let Some(shade) = element(doc, "div", "mn-sel") {
            set_box(&shade, rect.fix(), origin);
            append(&self.overlay, &shade);
        }
    }

    fn mark_caret(&self, doc: &Document, rect: Box2, origin: &web_sys::DomRect, primary: bool) {
        let Some(caret) = element(doc, "div", "mn-cursor") else {
            return;
        };
        if primary {
            caret.class_list().add_1("mn-cursor-primary").ok();
        }
        set_box(&caret, Box2 { width: 2.0, ..rect }.fix(), origin);
        append(&self.overlay, &caret);
    }

    fn selection_rects(&self, sel: Sel) -> Vec<Box2> {
        let (start, end) = (sel.start(), sel.end());
        let mut rects = Vec::new();
        for line in start.line..=end.line {
            let from = if line == start.line { start.col } else { 0 };
            let to = (line == end.line).then_some(end.col);
            if let Some(rect) = self.span_in_row(line, &[], from, to) {
                rects.push(rect);
            }
        }
        rects
    }

    /// The rectangle a selection covers in one row. It is as tall as what it
    /// spans, not as tall as a line: selecting a fraction shades the whole
    /// fraction, the way selecting a word shades the whole word.
    fn span_in_row(
        &self,
        line: usize,
        path: &Path,
        from: usize,
        to: Option<usize>,
    ) -> Option<Box2> {
        let row = self.row_element(line, path)?;
        let left = boundary(&row, from)?;
        let right = match to {
            Some(index) => boundary(&row, index)?,
            // Selecting past the end of a line shows the newline as a gap.
            None => {
                let mut rect = boundary(&row, usize::MAX)?;
                rect.left += 6.0;
                rect
            }
        };
        Some(span_box(&row, left, right))
    }

    fn caret_rect(&self, at: Pos) -> Option<Box2> {
        self.place_box(at.line, &[], at.col)
    }

    /// Where a place in a row is on screen. `usize::MAX` means the end of it.
    fn place_box(&self, line: usize, path: &Path, index: usize) -> Option<Box2> {
        let row = self.row_element(line, path)?;
        boundary(&row, index)
    }

    fn line_element(&self, line: usize) -> Option<Element> {
        self.lines.children().item(line as u32)
    }

    fn line_row(&self, line: usize) -> Option<Element> {
        self.row_element(line, &[])
    }

    fn row_element(&self, line: usize, path: &Path) -> Option<Element> {
        let holder = self.line_element(line)?;
        let selector = format!("[{PATH_ATTR}=\"{}\"]", row::encode_path(path));
        holder.query_selector(&selector).ok().flatten()
    }

    /// The place in the document a click landed on, at whatever depth: the
    /// innermost row the point is in decides, so clicking a denominator lands
    /// in the denominator and not beside the fraction.
    pub fn hit(&self, text: &Text, x: f64, y: f64) -> Hit {
        let mut line = text.line_count() - 1;
        for candidate in 0..text.line_count() {
            let Some(holder) = self.line_element(candidate) else {
                continue;
            };
            let rect = holder.get_bounding_client_rect();
            if y < rect.bottom() {
                line = candidate;
                break;
            }
        }
        let Some(holder) = self.line_element(line) else {
            return Hit::Text(Pos::new(line, 0));
        };
        let row = innermost_row(&holder, x, y);
        let Some(row) = row else {
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

    /// The place in the text a click landed on, taking an island as a whole.
    pub fn pos_at_point(&self, text: &Text, x: f64, y: f64) -> Pos {
        match self.hit(text, x, y) {
            Hit::Text(at) => at,
            // A point inside an island is at the island, and past it once the
            // pointer is on its right half, so dragging over one takes it in.
            Hit::Inside(at, _) => match self.field_box(at) {
                Some(rect) if x > rect.left + rect.width / 2.0 => Pos::new(at.line, at.col + 1),
                _ => at,
            },
        }
    }

    fn field_box(&self, at: Pos) -> Option<Box2> {
        let row = self.line_row(at.line)?;
        let children = row.children();
        for i in 0..children.length() {
            let child = children.item(i)?;
            if !child.class_list().contains(FIELD_CLASS) {
                continue;
            }
            if start_of(&child) == Some(at.col) {
                return Some(box_of(&child.get_bounding_client_rect()));
            }
        }
        None
    }

    /// Where the caret is drawn, be that a place in the text or a place inside
    /// the structure standing there.
    fn caret_box(&self, caret: &Caret<'_>) -> Option<Box2> {
        let (path, index) = caret.place();
        self.place_box(caret.at.line, &path, index)
    }

    /// Scrolls so the caret stays in sight, and reports where it is on screen so
    /// the input element can follow it (which is where IME candidates appear).
    pub fn reveal(&self, caret: &Caret<'_>) -> Option<Box2> {
        let rect = self.caret_box(caret)?;
        let view = box_of(&self.root.get_bounding_client_rect());
        let top = rect.top - view.top + self.root.scroll_top() as f64;
        let left = rect.left - view.left + self.root.scroll_left() as f64;
        if top < self.root.scroll_top() as f64 {
            self.root.set_scroll_top(top as i32);
        } else if top + rect.height > self.root.scroll_top() as f64 + view.height {
            self.root
                .set_scroll_top((top + rect.height - view.height) as i32);
        }
        if left < self.root.scroll_left() as f64 {
            self.root.set_scroll_left((left - 24.0).max(0.0) as i32);
        } else if left > self.root.scroll_left() as f64 + view.width - 24.0 {
            self.root.set_scroll_left((left - view.width + 24.0) as i32);
        }
        Some(Box2 {
            left: rect.left - view.left,
            top: rect.top - view.top,
            ..rect
        })
    }
}

#[derive(Clone, Copy, Default)]
pub struct Box2 {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

impl Box2 {
    fn fix(mut self) -> Self {
        if self.height <= 0.0 {
            self.height = 18.0;
        }
        if self.width <= 0.0 {
            self.width = 1.0;
        }
        self
    }
}

fn span_box(row: &Element, left: Box2, right: Box2) -> Box2 {
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
fn boundary(row: &Element, index: usize) -> Option<Box2> {
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

fn start_of(child: &Element) -> Option<usize> {
    child.get_attribute(START_ATTR)?.parse().ok()
}

fn box_of(rect: &web_sys::DomRect) -> Box2 {
    Box2 {
        left: rect.left(),
        top: rect.top(),
        width: rect.width(),
        height: rect.height(),
    }
}

fn set_box(element: &Element, rect: Box2, origin: &web_sys::DomRect) {
    let style = format!(
        "left:{}px;top:{}px;width:{}px;height:{}px",
        rect.left - origin.left(),
        rect.top - origin.top(),
        rect.width,
        rect.height
    );
    element.set_attribute("style", &style).ok();
}

fn has_tab(items: &[Item]) -> bool {
    items.contains(&Item::Tab)
}

fn children_of_class(holder: &Element, class: &str) -> Vec<Element> {
    let children = holder.children();
    (0..children.length())
        .filter_map(|i| children.item(i))
        .filter(|child| child.class_list().contains(class))
        .collect()
}

fn append(parent: &impl AsRef<web_sys::Node>, child: &Element) {
    parent.as_ref().append_child(child.as_ref()).ok();
}

fn element(doc: &Document, tag: &str, class: &str) -> Option<Element> {
    let element = doc.create_element(tag).ok()?;
    element.set_class_name(class);
    Some(element)
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
