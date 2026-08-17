//! Draws the document, the carets and the selections into the page.
//!
//! Every line is drawn by [`crate::view::row`], the one component that draws a
//! row, so a line of text and the inside of a structure are the same thing to
//! everything here. The only thing a line does that a row inside a structure
//! cannot is line its column separators up with its neighbours.
//!
//! Where the drawn things ended up is read back by [`crate::view::measure`]:
//! putting something on screen and measuring what is there are opposite jobs,
//! so they are kept apart.
//!
//! Nothing here is editable by the browser: lines are plain spans, and every
//! caret is a small absolutely placed element, which is how several carets can
//! be shown at once, and how a caret inside a structure is the same caret as a
//! caret in the text.
//!
//! Only the lines that can be seen are in the page. Above and below them sit
//! two empty elements as tall as the lines they stand for, so the document
//! scrolls its full length while the page holds a screenful of it. A line's
//! height is kept once measured; a line never drawn is guessed at, which is why
//! the scrollbar settles as the document is scrolled through.

use std::cell::RefCell;
use std::ops::Range;

use web_sys::{Document, Element, HtmlElement};

use crate::structure::ast::Cursor;
use crate::structure::text::{Item, Pos, Sel, Text};
use crate::view::measure::{self, Box2, Hit};
use crate::view::row::{self, Path, Preedit, Renderer, FIELD_CLASS, PATH_ATTR, TAB_CLASS};

pub const LINE_CLASS: &str = "mn-line";
const LINE_ATTR: &str = "data-line";
/// The number shown beside a line. It sits in the margin, outside the row, so
/// that nothing measuring the text can mistake it for part of the line.
const NUMBER_CLASS: &str = "mn-number";
/// Stands for the lines that are not in the page, so the document keeps its
/// full length.
const GAP_CLASS: &str = "mn-gap";

/// How far past the screen lines are drawn, in screens. Scrolling by less than
/// this shows lines that are already there.
const MARGIN_SCREENS: f64 = 1.0;
/// What a line is taken to be worth before it has ever been measured.
const GUESS: f64 = 20.0;
/// How far a block of column separators may pull the drawn range past the
/// screen. Lining columns up needs the whole block, but not at any price.
const BLOCK_LIMIT: usize = 200;

pub struct View {
    /// Scrolls, and receives the mouse.
    pub root: HtmlElement,
    lines: Element,
    overlay: Element,
    /// Every line's height as last measured; `0.0` for a line never drawn.
    heights: RefCell<Vec<f64>>,
    /// The lines that are in the page right now. Everything that measures the
    /// page can only speak about these.
    drawn: RefCell<Range<usize>>,
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
            heights: RefCell::new(Vec::new()),
            drawn: RefCell::new(0..0),
        })
    }

    /// Draws after a change, bringing the caret's line into the page.
    pub fn draw(&self, text: &Text, sels: &[Sel], caret: &Caret<'_>, focused: bool) {
        self.paint(text, sels, caret, focused, true);
    }

    /// Draws after a scroll, which must not move the view to the caret: the
    /// scrollbar is the user's, not the caret's.
    pub fn repaint(&self, text: &Text, sels: &[Sel], caret: &Caret<'_>, focused: bool) {
        self.paint(text, sels, caret, focused, false);
    }

    fn paint(
        &self,
        text: &Text,
        sels: &[Sel],
        caret: &Caret<'_>,
        focused: bool,
        follow_caret: bool,
    ) {
        let Some(doc) = self.lines.owner_document() else {
            return;
        };
        self.fit_heights(text.line_count());
        if follow_caret {
            self.scroll_to_line(caret.at.line);
        }
        let window = self.widen_for_blocks(text, self.window(text));
        // Text an IME is composing belongs at the caret, whichever row that is
        // in: the component that draws the row puts it there.
        let (path, index) = caret.place();
        let preedit = caret.composing.map(|text| Preedit { path, index, text });
        self.lines.set_inner_html("");
        let above = element(&doc, "div", GAP_CLASS);
        if let Some(gap) = &above {
            append(&self.lines, gap);
        }
        for line in window.clone() {
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
        let below = element(&doc, "div", GAP_CLASS);
        if let Some(gap) = &below {
            append(&self.lines, gap);
        }
        *self.drawn.borrow_mut() = window.clone();
        // Measured first, so the two gaps stand for what the lines around them
        // are really worth.
        self.measure(&window);
        if let Some(gap) = &above {
            set_height(gap, self.span(0..window.start));
        }
        if let Some(gap) = &below {
            set_height(gap, self.span(window.end..text.line_count()));
        }
        self.align_columns(text, &window);
        self.draw_carets(&doc, sels, caret, focused);
    }

    /// Keeps one height per line, so that a line's measurement outlives the
    /// time it spends off screen.
    fn fit_heights(&self, count: usize) {
        let mut heights = self.heights.borrow_mut();
        if heights.len() != count {
            heights.resize(count, 0.0);
        }
    }

    /// What a stretch of lines is worth: measured where it has been drawn, and
    /// guessed at where it has not.
    fn span(&self, lines: Range<usize>) -> f64 {
        let heights = self.heights.borrow();
        let unit = unit_height(&heights);
        lines
            .map(|line| match heights.get(line) {
                Some(height) if *height > 0.0 => *height,
                _ => unit,
            })
            .sum()
    }

    /// The lines the screen reaches, plus a screen above and below so that
    /// scrolling a little needs no drawing at all.
    fn window(&self, text: &Text) -> Range<usize> {
        let count = text.line_count();
        let height = self.root.client_height() as f64;
        let margin = (height * MARGIN_SCREENS).max(200.0);
        let scroll = self.root.scroll_top() as f64;
        let (top, bottom) = (scroll - margin, scroll + height + margin);
        let mut start = 0;
        let mut y = 0.0;
        while start + 1 < count {
            let next = y + self.span(start..start + 1);
            if next >= top {
                break;
            }
            y = next;
            start += 1;
        }
        let mut end = start;
        while end < count && y <= bottom {
            y += self.span(end..end + 1);
            end += 1;
        }
        start..end.max(start + 1).min(count)
    }

    /// Widens the drawn range to whole blocks of column separators: lining a
    /// column up is about the block, so a block cannot be drawn in halves.
    fn widen_for_blocks(&self, text: &Text, window: Range<usize>) -> Range<usize> {
        let count = text.line_count();
        let mut start = window.start;
        let floor = start.saturating_sub(BLOCK_LIMIT);
        while start > floor && has_tab(text.line(start)) && has_tab(text.line(start - 1)) {
            start -= 1;
        }
        let mut end = window.end;
        let ceiling = (end + BLOCK_LIMIT).min(count);
        while end < ceiling && has_tab(text.line(end - 1)) && has_tab(text.line(end)) {
            end += 1;
        }
        start..end
    }

    /// Notes how tall the lines just drawn turned out to be.
    fn measure(&self, window: &Range<usize>) {
        let mut heights = self.heights.borrow_mut();
        for line in window.clone() {
            let Some(holder) = self.line_element(line) else {
                continue;
            };
            let height = holder.get_bounding_client_rect().height();
            if height > 0.0 {
                if let Some(slot) = heights.get_mut(line) {
                    *slot = height;
                }
            }
        }
    }

    /// Brings a line into the screen when it is not there, which is what makes
    /// a caret that jumped far (Ctrl+End, a search hit) reachable at all: only
    /// drawn lines can be measured, so the view has to move first.
    fn scroll_to_line(&self, line: usize) {
        let height = self.root.client_height() as f64;
        let scroll = self.root.scroll_top() as f64;
        let top = self.span(0..line);
        let bottom = top + self.span(line..line + 1);
        if top >= scroll && bottom <= scroll + height {
            return;
        }
        // A third of the way down, so that what follows the caret is visible.
        self.root
            .set_scroll_top((top - height / 3.0).max(0.0) as i32);
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
        if crate::settings::line_numbers() {
            if let Some(number) = element(doc, "span", NUMBER_CLASS) {
                number.set_text_content(Some(&(line + 1).to_string()));
                append(&holder, &number);
            }
        }
        let renderer = Renderer::new(doc).with_preedit(preedit);
        append(&holder, &renderer.line(text.line(line)));
        Some(holder)
    }

    /// Lines up the column separators of neighbouring lines. This is the one
    /// thing a line does that a row inside a structure does not: it is about
    /// several lines at once, which only the document has.
    fn align_columns(&self, text: &Text, window: &Range<usize>) {
        let mut line = window.start;
        while line < window.end {
            if !has_tab(text.line(line)) {
                line += 1;
                continue;
            }
            let mut end = line;
            while end < window.end && has_tab(text.line(end)) {
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
                let width = (widest - left + crate::settings::column_gap()).max(1.0);
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
                for rect in
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
            rects.extend(self.span_in_row(line, &[], from, to));
        }
        rects
    }

    /// The rectangles a selection covers in one row. Usually one, but a line
    /// carried on underneath is covered a piece at a time, one per carried
    /// line. Each is as tall as what it spans, not as tall as a line: selecting
    /// a fraction shades the whole fraction, the way selecting a word shades
    /// the whole word.
    fn span_in_row(&self, line: usize, path: &Path, from: usize, to: Option<usize>) -> Vec<Box2> {
        let Some(row) = self.row_element(line, path) else {
            return Vec::new();
        };
        // Selecting past the end of a line shows the newline as a gap.
        let past_end = to.is_none();
        measure::span_boxes(&row, from, to.unwrap_or(usize::MAX), past_end)
    }

    fn caret_rect(&self, at: Pos) -> Option<Box2> {
        self.place_box(at.line, &[], at.col)
    }

    /// Where a place in a row is on screen. `usize::MAX` means the end of it.
    fn place_box(&self, line: usize, path: &Path, index: usize) -> Option<Box2> {
        let row = self.row_element(line, path)?;
        measure::boundary(&row, index)
    }

    /// A line's element, by the line it stands for rather than by its place
    /// among the children: only some of the lines are in the page.
    fn line_element(&self, line: usize) -> Option<Element> {
        self.lines
            .query_selector(&format!("[{LINE_ATTR}=\"{line}\"]"))
            .ok()
            .flatten()
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
        // Only what is in the page can be hit, so a click is answered with the
        // drawn lines: everything else is not under the pointer anyway.
        let drawn = self.drawn.borrow().clone();
        let last = text.line_count() - 1;
        let mut line = drawn.end.saturating_sub(1).min(last);
        for candidate in drawn {
            let Some(holder) = self.line_element(candidate) else {
                continue;
            };
            if y < holder.get_bounding_client_rect().bottom() {
                line = candidate;
                break;
            }
        }
        match self.line_element(line) {
            Some(holder) => measure::hit_in_line(&holder, line, x, y),
            None => Hit::Text(Pos::new(line, 0)),
        }
    }

    /// The place in the text a click landed on, taking an island as a whole.
    pub fn pos_at_point(&self, text: &Text, x: f64, y: f64) -> Pos {
        match self.hit(text, x, y) {
            Hit::Text(at) => at,
            // A point inside an island is at the island, and past it once the
            // pointer is on its right half, so dragging over one takes it in.
            Hit::Inside(at, _) => match self.island_box(at) {
                Some(rect) if x > rect.left + rect.width / 2.0 => Pos::new(at.line, at.col + 1),
                _ => at,
            },
        }
    }

    fn island_box(&self, at: Pos) -> Option<Box2> {
        let row = self.line_row(at.line)?;
        let children = row.children();
        for i in 0..children.length() {
            let child = children.item(i)?;
            if !child.class_list().contains(FIELD_CLASS) {
                continue;
            }
            if measure::start_of(&child) == Some(at.col) {
                return Some(measure::box_of(&child.get_bounding_client_rect()));
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
        let view = measure::box_of(&self.root.get_bounding_client_rect());
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

fn set_height(element: &Element, height: f64) {
    element
        .set_attribute("style", &format!("height:{height}px"))
        .ok();
}

/// What an unmeasured line is taken to be worth: what the measured ones are,
/// on average, so a document of tall lines is not guessed at as short ones.
fn unit_height(heights: &[f64]) -> f64 {
    let mut total = 0.0;
    let mut seen = 0;
    for height in heights.iter().filter(|height| **height > 0.0) {
        total += *height;
        seen += 1;
    }
    if seen == 0 {
        return GUESS;
    }
    total / seen as f64
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
