//! Draws the document, the carets and the selections into the page.
//!
//! Nothing here is editable by the browser: lines are plain spans, and every
//! caret is a small absolutely placed element, which is how several carets can
//! be shown at once.

use web_sys::{Document, Element, HtmlElement, Range};

use super::model::{Item, Pos, Sel, Text};
use crate::math::ast::Cursor;
use crate::math::notation::parse_island;
use crate::math::render;

pub const LINE_CLASS: &str = "mn-line";
pub const RUN_CLASS: &str = "mn-run";
pub const FIELD_CLASS: &str = "mn-field";
const TAB_CLASS: &str = "mn-tab";
const PREEDIT_CLASS: &str = "mn-preedit";
/// The gap left after the widest cell of a column.
const COLUMN_GAP: f64 = 18.0;
const LINE_ATTR: &str = "data-line";
const COL_ATTR: &str = "data-col";

pub struct View {
    /// Scrolls, and receives the mouse.
    pub root: HtmlElement,
    lines: Element,
    overlay: Element,
}

/// Where a formula is being edited, so it can be drawn with its own caret.
pub struct ActiveMath<'a> {
    pub at: Pos,
    pub cursor: &'a Cursor,
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

    pub fn draw(
        &self,
        text: &Text,
        sels: &[Sel],
        active: Option<ActiveMath<'_>>,
        focused: bool,
        preedit: Option<(Pos, &str)>,
    ) {
        let Some(doc) = self.lines.owner_document() else {
            return;
        };
        self.lines.set_inner_html("");
        for line in 0..text.line_count() {
            let composing = preedit.filter(|(at, _)| at.line == line);
            if let Some(element) = self.draw_line(&doc, text, line, active.as_ref(), composing) {
                append(&self.lines, &element);
            }
        }
        self.align_columns(text);
        // While an IME is composing, the underlined text stands in for the caret.
        self.draw_carets(&doc, sels, focused && active.is_none() && preedit.is_none());
    }

    fn draw_line(
        &self,
        doc: &Document,
        text: &Text,
        line: usize,
        active: Option<&ActiveMath<'_>>,
        preedit: Option<(Pos, &str)>,
    ) -> Option<Element> {
        let holder = element(doc, "div", LINE_CLASS)?;
        holder.set_attribute(LINE_ATTR, &line.to_string()).ok();
        let mut run = String::new();
        let mut run_start = 0usize;
        let items = text.line(line);
        for (col, item) in items.iter().enumerate() {
            if let Some((_, composing)) = preedit.filter(|(at, _)| at.col == col) {
                if !run.is_empty() {
                    append(&holder, &run_element(doc, &run, run_start)?);
                    run.clear();
                }
                append(&holder, &preedit_element(doc, composing)?);
            }
            match item {
                Item::Char(c) => {
                    if run.is_empty() {
                        run_start = col;
                    }
                    run.push(*c);
                }
                Item::Tab => {
                    if !run.is_empty() {
                        append(&holder, &run_element(doc, &run, run_start)?);
                        run.clear();
                    }
                    let tab = element(doc, "span", TAB_CLASS)?;
                    tab.set_attribute(COL_ATTR, &col.to_string()).ok();
                    append(&holder, &tab);
                }
                Item::Math { source } => {
                    if !run.is_empty() {
                        append(&holder, &run_element(doc, &run, run_start)?);
                        run.clear();
                    }
                    let cursor = active
                        .filter(|active| active.at == Pos::new(line, col))
                        .map(|active| active.cursor);
                    let field = element(doc, "span", FIELD_CLASS)?;
                    field.set_attribute(COL_ATTR, &col.to_string()).ok();
                    if cursor.is_some() {
                        field.class_list().add_1("mn-field-active").ok();
                    }
                    let row = parse_island(source);
                    if row.is_empty() {
                        field.class_list().add_1("mn-field-empty").ok();
                    }
                    render::render_into(&field, &row, cursor);
                    append(&holder, &field);
                }
            }
        }
        if !run.is_empty() {
            append(&holder, &run_element(doc, &run, run_start)?);
        }
        if let Some((_, composing)) = preedit.filter(|(at, _)| at.col >= items.len()) {
            append(&holder, &preedit_element(doc, composing)?);
        }
        if items.is_empty() {
            // An empty line still needs a box to measure and click on.
            append(&holder, &run_element(doc, "", 0)?);
        }
        Some(holder)
    }

    /// Lines up the column separators of neighbouring lines. Only the drawing is
    /// touched: the text keeps one separator per `$(t)`, wherever it sits.
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
            .map(|line| match self.line_element(line) {
                Some(holder) => children_of_class(&holder, TAB_CLASS),
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

    fn draw_carets(&self, doc: &Document, sels: &[Sel], show_carets: bool) {
        self.overlay.set_inner_html("");
        let origin = self.lines.get_bounding_client_rect();
        for (index, sel) in sels.iter().enumerate() {
            if !sel.is_caret() {
                for rect in self.selection_rects(*sel) {
                    if let Some(shade) = element(doc, "div", "mn-sel") {
                        set_box(&shade, rect, &origin);
                        append(&self.overlay, &shade);
                    }
                }
            }
            if !show_carets {
                continue;
            }
            if let Some(rect) = self.caret_rect(sel.head) {
                if let Some(caret) = element(doc, "div", "mn-cursor") {
                    if index + 1 == sels.len() {
                        caret.class_list().add_1("mn-cursor-primary").ok();
                    }
                    set_box(&caret, Box2 { width: 2.0, ..rect }, &origin);
                    append(&self.overlay, &caret);
                }
            }
        }
    }

    fn selection_rects(&self, sel: Sel) -> Vec<Box2> {
        let (start, end) = (sel.start(), sel.end());
        let mut rects = Vec::new();
        for line in start.line..=end.line {
            let Some(holder) = self.line_element(line) else {
                continue;
            };
            let from = if line == start.line { start.col } else { 0 };
            let to = if line == end.line {
                Some(end.col)
            } else {
                None
            };
            let Some(left) = self.boundary(&holder, from) else {
                continue;
            };
            let right = match to {
                Some(col) => self.boundary(&holder, col),
                // Selecting past the end of a line shows the newline as a gap.
                None => self.boundary(&holder, usize::MAX).map(|mut rect| {
                    rect.left += 6.0;
                    rect
                }),
            };
            let Some(right) = right else { continue };
            rects.push(
                Box2 {
                    left: left.left,
                    top: right.top.min(left.top),
                    width: (right.left - left.left).max(1.0),
                    height: right.height.max(left.height),
                }
                .fix(),
            );
        }
        rects
    }

    fn caret_rect(&self, at: Pos) -> Option<Box2> {
        let holder = self.line_element(at.line)?;
        self.boundary(&holder, at.col)
    }

    /// The place just before the item at `col`; `usize::MAX` means end of line.
    fn boundary(&self, holder: &Element, col: usize) -> Option<Box2> {
        let children = holder.children();
        let mut last: Option<Box2> = None;
        for i in 0..children.length() {
            let child = children.item(i)?;
            if child.class_list().contains(PREEDIT_CLASS) {
                continue;
            }
            let start: usize = child
                .get_attribute(COL_ATTR)
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            if child.class_list().contains(FIELD_CLASS) || child.class_list().contains(TAB_CLASS) {
                let rect = box_of(&child.get_bounding_client_rect());
                if col == start {
                    return Some(rect);
                }
                last = Some(Box2 {
                    left: rect.left + rect.width,
                    ..rect
                });
                continue;
            }
            let text = child.text_content().unwrap_or_default();
            let len = text.chars().count();
            if col >= start && col <= start + len {
                return text_boundary(&child, col - start).or(last);
            }
            last = text_boundary(&child, len).or(last);
        }
        last
    }

    fn line_element(&self, line: usize) -> Option<Element> {
        self.lines.children().item(line as u32)
    }

    /// The place in the document a click landed on.
    pub fn pos_at_point(&self, text: &Text, x: f64, y: f64) -> Pos {
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
            return Pos::new(line, 0);
        };
        let mut best = (f64::MAX, 0usize);
        for col in 0..=text.line_len(line) {
            if let Some(rect) = self.boundary(&holder, col) {
                let distance = (rect.left - x).abs();
                if distance < best.0 {
                    best = (distance, col);
                }
            }
        }
        Pos::new(line, best.1)
    }

    /// The formula element the point is over, so a click can enter it.
    pub fn field_at_point(&self, x: f64, y: f64) -> Option<(Pos, Element)> {
        let doc = self.root.owner_document()?;
        let target = doc.element_from_point(x as f32, y as f32)?;
        let field = target.closest(&format!(".{FIELD_CLASS}")).ok()??;
        let line = field
            .closest(&format!(".{LINE_CLASS}"))
            .ok()??
            .get_attribute(LINE_ATTR)?
            .parse()
            .ok()?;
        let col = field.get_attribute(COL_ATTR)?.parse().ok()?;
        Some((Pos::new(line, col), field))
    }

    /// Scrolls so the caret stays in sight, and reports where it is on screen so
    /// the input element can follow it (which is where IME candidates appear).
    pub fn reveal(&self, at: Pos) -> Option<Box2> {
        let rect = self.caret_rect(at)?;
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
        self
    }
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

/// The text an IME is still composing, shown inline where it will land.
fn preedit_element(doc: &Document, text: &str) -> Option<Element> {
    let element = element(doc, "span", PREEDIT_CLASS)?;
    element.set_text_content(Some(text));
    Some(element)
}

fn run_element(doc: &Document, run: &str, start: usize) -> Option<Element> {
    let element = element(doc, "span", RUN_CLASS)?;
    element.set_attribute(COL_ATTR, &start.to_string()).ok();
    element.set_text_content(Some(run));
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
    let Some(line) = run.parent_element() else {
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
