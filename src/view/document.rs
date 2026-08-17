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
use crate::view::heights::Heights;
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
/// How far a block of column separators may pull the drawn range past the
/// screen. Lining columns up needs the whole block, but not at any price.
const BLOCK_LIMIT: usize = 200;

pub struct View {
    /// Scrolls, and receives the mouse.
    pub root: HtmlElement,
    lines: Element,
    overlay: Element,
    /// How tall every line is, which is what decides the lines to draw.
    heights: RefCell<Heights>,
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
            heights: RefCell::new(Heights::new()),
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
        self.heights.borrow_mut().fit(text.line_count());
        // Where the view is going to be: at the caret when something changed,
        // and where the user left it when they scrolled.
        let mut scroll = match follow_caret {
            true => self.scroll_for(caret.at.line, text.line_count()),
            false => self.root.scroll_top() as f64,
        };
        // Whether the caret's line can be seen right now. A draw that follows
        // the caret has to end with it in sight; a draw that does not must at
        // least not lose sight of it, which measuring the lines anew can do at
        // the end of the document, where the scroll cannot move as far as the
        // lines it has to make room for.
        let mut keep = follow_caret;
        if !follow_caret {
            let window = self.widen_for_blocks(text, self.window(scroll, text.line_count()));
            // Scrolling within the margin shows lines that are already there,
            // so there is nothing to draw.
            if window == *self.drawn.borrow() {
                return;
            }
            keep = (self.scroll_onto(caret.at.line, scroll) - scroll).abs() <= 0.5;
        }
        scroll = self.render(text, sels, caret, focused, scroll);
        if !keep {
            return;
        }
        // The scroll above was worked out from heights that were still guesses
        // for the lines it was about to draw, so the caret's line can end up
        // somewhere else than the guess put it. Where the view ends up is
        // therefore settled against the lines as they were drawn, until it
        // stops moving: a guess is never what the user is left looking at.
        for _ in 0..3 {
            let settled = self.scroll_onto(caret.at.line, scroll);
            if (settled - scroll).abs() <= 0.5 {
                return;
            }
            scroll = self.render(text, sels, caret, focused, settled);
        }
    }

    /// The scroll that shows a line whole, or `scroll` if the line is in sight
    /// there already. The line as drawn is what is measured, so a guess about
    /// the lines above it cannot move the answer. A line brought into sight is
    /// brought a line further than it needs, because a line flush with the edge
    /// of the view reads as one that is not there; at the end of the document
    /// the browser trims the extra, where the padding leaves room anyway.
    fn scroll_onto(&self, line: usize, scroll: f64) -> f64 {
        let view = self.root.client_height() as f64;
        let Some(holder) = self.line_element(line) else {
            // The line the view is being moved for is not even drawn, so the
            // heights that chose the range were wrong about it. Aim at it once
            // more from what has been measured since.
            return (self.heights.borrow().top_of(line) - view / 3.0).max(0.0);
        };
        let rect = measure::box_of(&holder.get_bounding_client_rect());
        let root = measure::box_of(&self.root.get_bounding_client_rect());
        // What is on screen, said in the scroll's own measure.
        let top = rect.top - root.top + scroll;
        let bottom = top + rect.height;
        if top < scroll {
            (top - rect.height).max(0.0)
        } else if bottom > scroll + view {
            bottom + rect.height - view
        } else {
            scroll
        }
    }

    /// Puts the lines for `scroll` in the page and leaves the view there,
    /// giving back where it actually ended up.
    fn render(
        &self,
        text: &Text,
        sels: &[Sel],
        caret: &Caret<'_>,
        focused: bool,
        scroll: f64,
    ) -> f64 {
        let Some(doc) = self.lines.owner_document() else {
            return self.root.scroll_top() as f64;
        };
        let window = self.widen_for_blocks(text, self.window(scroll, text.line_count()));
        // Text an IME is composing belongs at the caret, whichever row that is
        // in: the component that draws the row puts it there.
        let (path, index) = caret.place();
        let preedit = caret.composing.map(|text| Preedit { path, index, text });
        self.lines.set_inner_html("");
        let above = element(&doc, "div", GAP_CLASS);
        // What the lines above the drawn ones were taken to be worth. Measuring
        // changes it, and the drawn lines move by the difference, so the scroll
        // has to move with them.
        let guessed = self.heights.borrow().span(0..window.start);
        if let Some(gap) = &above {
            // The gaps are given their height before they are in the page: a
            // page that is briefly shorter than the document is a page the
            // browser trims the scroll of, which would stop the view from
            // going any further down.
            set_height(gap, guessed);
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
            set_height(
                gap,
                self.heights.borrow().span(window.end..text.line_count()),
            );
            append(&self.lines, gap);
        }
        *self.drawn.borrow_mut() = window.clone();
        self.measure(&window);
        // The gaps again, now that the lines between them have been measured.
        let heights = self.heights.borrow();
        let measured = heights.span(0..window.start);
        if let Some(gap) = &above {
            set_height(gap, measured);
        }
        if let Some(gap) = &below {
            set_height(gap, heights.span(window.end..text.line_count()));
        }
        drop(heights);
        // Where the view ends up is decided here and nowhere else. Replacing
        // the lines can leave the scroll trimmed to a page that was shorter for
        // a moment, and a scroll that comes back trimmed is a view that cannot
        // be scrolled past the lines that happen to be drawn. The gap above
        // growing or shrinking as the lines are measured moves the lines with
        // it, and moving the scroll by the same amount is what keeps what is on
        // screen where it was.
        let scroll = (scroll + measured - guessed).max(0.0);
        if (self.root.scroll_top() as f64 - scroll).abs() > 0.5 {
            self.root.set_scroll_top(scroll as i32);
        }
        self.align_columns(text, &window);
        self.draw_carets(&doc, sels, caret, focused);
        // What the browser gave, not what was asked for: the end of the
        // document is as far as it goes.
        self.root.scroll_top() as f64
    }

    /// The lines the screen reaches, plus a screen above and below so that
    /// scrolling a little needs no drawing at all.
    fn window(&self, scroll: f64, count: usize) -> Range<usize> {
        let height = self.root.client_height() as f64;
        let margin = (height * MARGIN_SCREENS).max(200.0);
        let heights = self.heights.borrow();
        let start = heights.line_at(scroll - margin);
        let end = heights.line_at(scroll + height + margin) + 1;
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
                heights.set(line, height);
            }
        }
    }

    /// Where the view has to be for a line to be drawn at all. A line the
    /// coming range already reaches leaves the view alone: moving to the caret
    /// a line at a time is [`Self::reveal`]'s job. A line further off than that
    /// (Ctrl+End, a search hit, a long paste) is what this is for, because an
    /// undrawn line cannot be measured, so the view has to move first.
    fn scroll_for(&self, line: usize, count: usize) -> f64 {
        let scroll = self.root.scroll_top() as f64;
        if self.window(scroll, count).contains(&line) {
            return scroll;
        }
        let height = self.root.client_height() as f64;
        // A third of the way down, so that what follows the caret is visible.
        (self.heights.borrow().top_of(line) - height / 3.0).max(0.0)
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

    /// Scrolls so the caret stays in sight, and reports where it is **in the
    /// document** so the input element can follow it (which is where IME
    /// candidates appear). Document, not screen: the input element sits among
    /// the lines and scrolls with them, and an input element left behind at the
    /// top of the document is one the browser scrolls back to as soon as it is
    /// typed into.
    pub fn reveal(&self, caret: &Caret<'_>) -> Option<Box2> {
        let rect = self.caret_box(caret)?;
        let view = measure::box_of(&self.root.get_bounding_client_rect());
        let scroll = (
            self.root.scroll_top() as f64,
            self.root.scroll_left() as f64,
        );
        let top = rect.top - view.top + scroll.0;
        let left = rect.left - view.left + scroll.1;
        // What can be seen is the client box: the room a scrollbar takes is not
        // room a caret can be seen in.
        let height = self.root.client_height() as f64;
        let width = self.root.client_width() as f64;
        if top < scroll.0 {
            self.root.set_scroll_top(top as i32);
        } else if top + rect.height > scroll.0 + height {
            self.root
                .set_scroll_top((top + rect.height - height) as i32);
        }
        if left < scroll.1 {
            self.root.set_scroll_left((left - 24.0).max(0.0) as i32);
        } else if left > scroll.1 + width - 24.0 {
            self.root.set_scroll_left((left - width + 24.0) as i32);
        }
        Some(Box2 { left, top, ..rect })
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
