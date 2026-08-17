//! The one component that draws a row.
//!
//! A row is a row wherever it is: a line of the document, the inside of an
//! island, the numerator of a fraction. They are all drawn by this component,
//! so a character, an IME's uncommitted text and a nested structure behave the
//! same at every depth. A row knows where it is from its path, and that is the
//! only thing that differs: the path of a line of the document is empty, and a
//! line is the only row that accepts a column separator.

use web_sys::{Document, Element};

use crate::structure::ast::{Between, Delim, MatrixKind, Node, Row};
use crate::structure::text::Item;
use crate::structure::vocabulary::{self as vocabulary, Class};

const SVG_NS: &str = "http://www.w3.org/2000/svg";

pub const ROW_CLASS: &str = "mn-row";
pub const RUN_CLASS: &str = "mn-run";
pub const FIELD_CLASS: &str = "mn-field";
pub const TAB_CLASS: &str = "mn-tab";
pub const PREEDIT_CLASS: &str = "mn-preedit";
/// The box an empty row shows so that it can be seen and clicked on.
pub const PLACEHOLDER_CLASS: &str = "mn-placeholder";
/// The path of the row an element is, written on every row.
pub const PATH_ATTR: &str = "data-path";
/// The place in the row an element starts at, written on everything in a row.
pub const START_ATTR: &str = "data-start";

/// Where a row is: the slots to walk through to reach it. Slots are `(the place
/// of the thing in its row, which of its rows)`, so `1.0,2.1` is the second row
/// of the third thing of the first row of the second thing of the line.
pub type Path = [(usize, usize)];

pub fn encode_path(path: &Path) -> String {
    path.iter()
        .map(|(index, slot)| format!("{index}.{slot}"))
        .collect::<Vec<_>>()
        .join(",")
}

pub fn decode_path(encoded: &str) -> Option<Vec<(usize, usize)>> {
    encoded
        .split(',')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let (index, slot) = part.split_once('.')?;
            Some((index.parse().ok()?, slot.parse().ok()?))
        })
        .collect()
}

/// One thing in a row. A line of the document and a slot inside a structure
/// hold different things, and this is where that difference ends.
pub enum Cell<'a> {
    Char(char),
    /// A column separator, which only a line of the document has.
    Tab,
    /// An island: a piece of a line that needs two dimensions.
    Field(&'a Row),
    /// Something inside a formula.
    Node(&'a Node),
}

pub fn cells_of_line(items: &[Item]) -> Vec<Cell<'_>> {
    items
        .iter()
        .map(|item| match item {
            Item::Char(c) => Cell::Char(*c),
            Item::Tab => Cell::Tab,
            Item::Math(row) => Cell::Field(row),
        })
        .collect()
}

pub fn cells_of_row(row: &Row) -> Vec<Cell<'_>> {
    row.iter().map(Cell::Node).collect()
}

/// Text an IME has not committed yet, and the place in the document it will
/// land in: the row, and how far into it.
pub struct Preedit<'a> {
    pub path: Vec<(usize, usize)>,
    pub index: usize,
    pub text: &'a str,
}

pub struct Renderer<'a> {
    doc: &'a Document,
    preedit: Option<&'a Preedit<'a>>,
}

/// Characters waiting to be drawn as one span, which is how text keeps its
/// spacing and its shape.
struct Run {
    start: usize,
    class: &'static str,
    text: String,
}

impl<'a> Renderer<'a> {
    pub fn new(doc: &'a Document) -> Renderer<'a> {
        Renderer { doc, preedit: None }
    }

    pub fn with_preedit(mut self, preedit: Option<&'a Preedit<'a>>) -> Renderer<'a> {
        self.preedit = preedit.filter(|preedit| !preedit.text.is_empty());
        self
    }

    /// Draws a line of the document. A line is the row whose path is empty.
    pub fn line(&self, items: &[Item]) -> Element {
        self.row(&cells_of_line(items), &[])
    }

    /// Draws one row, wherever it is.
    pub fn row(&self, cells: &[Cell<'_>], path: &Path) -> Element {
        // A line of the document is prose; anything inside an island is a
        // formula, and its letters are set the way a formula's are.
        let math = !path.is_empty();
        let container = self.el("span", ROW_CLASS);
        container.set_attribute(PATH_ATTR, &encode_path(path)).ok();
        if cells.is_empty() {
            container.append_child(&self.empty(math)).ok();
        }
        let mut run: Option<Run> = None;
        for (index, cell) in cells.iter().enumerate() {
            if let Some(preedit) = self.preedit_at(path, index) {
                self.flush(&container, &mut run);
                container.append_child(&preedit).ok();
            }
            match cell {
                Cell::Char(c) => {
                    let class = char_class(*c, math);
                    match run.as_mut().filter(|run| run.class == class) {
                        Some(run) => run.text.push(*c),
                        None => {
                            self.flush(&container, &mut run);
                            run = Some(Run {
                                start: index,
                                class,
                                text: c.to_string(),
                            });
                        }
                    }
                }
                cell => {
                    self.flush(&container, &mut run);
                    let element = self.cell(cell, path, index);
                    element.set_attribute(START_ATTR, &index.to_string()).ok();
                    container.append_child(&element).ok();
                }
            }
        }
        self.flush(&container, &mut run);
        if let Some(preedit) = self.preedit_at(path, cells.len()) {
            container.append_child(&preedit).ok();
        }
        container
    }

    /// An empty row still needs something to measure and to click on.
    fn empty(&self, math: bool) -> Element {
        let class = if math { PLACEHOLDER_CLASS } else { RUN_CLASS };
        let element = self.el("span", class);
        element.set_attribute(START_ATTR, "0").ok();
        element
    }

    fn preedit_at(&self, path: &Path, index: usize) -> Option<Element> {
        let preedit = self
            .preedit
            .filter(|preedit| preedit.path == path && preedit.index == index)?;
        Some(self.span(PREEDIT_CLASS, preedit.text))
    }

    fn flush(&self, container: &Element, run: &mut Option<Run>) {
        let Some(run) = run.take() else { return };
        let element = self.span(run.class, &run.text);
        element
            .set_attribute(START_ATTR, &run.start.to_string())
            .ok();
        container.append_child(&element).ok();
    }

    fn cell(&self, cell: &Cell<'_>, path: &Path, index: usize) -> Element {
        match cell {
            // Handled by the run above; a run is never a cell of its own.
            Cell::Char(c) => self.span(char_class(*c, !path.is_empty()), &c.to_string()),
            Cell::Tab => self.el("span", TAB_CLASS),
            Cell::Field(row) => {
                let field = self.el("span", FIELD_CLASS);
                if row.is_empty() {
                    field.class_list().add_1("mn-field-empty").ok();
                }
                let mut child = path.to_vec();
                child.push((index, 0));
                field
                    .append_child(&self.row(&cells_of_row(row), &child))
                    .ok();
                field
            }
            Cell::Node(node) => self.node(node, path, index),
        }
    }

    fn child_row(&self, node: &Node, slot: usize, path: &Path, index: usize) -> Element {
        let mut child = path.to_vec();
        child.push((index, slot));
        let row = node.slot(slot).cloned().unwrap_or_default();
        self.row(&cells_of_row(&row), &child)
    }

    fn node(&self, node: &Node, path: &Path, index: usize) -> Element {
        match node {
            Node::Char(c) => self.span(char_class(*c, true), &c.to_string()),
            Node::Sym(name) => {
                let symbol = vocabulary::lookup(name);
                let glyph = symbol.map(|s| s.glyph).unwrap_or(name.as_str());
                let class = match symbol.map(|s| s.class) {
                    Some(Class::Ident) => "mn-atom mn-ident",
                    Some(Class::Bin) => "mn-atom mn-bin",
                    Some(Class::Rel) => "mn-atom mn-rel",
                    _ => "mn-atom mn-punct",
                };
                self.span(class, glyph)
            }
            Node::Func(name) => self.span("mn-atom mn-func", name),
            Node::Stack { between, .. } => {
                // Equal halves above and below the rule put the rule in the
                // middle of the box, and the middle is what the row aligns on.
                let frac = self.el("span", "mn-frac");
                let num = self.el("span", "mn-frac-num");
                num.append_child(&self.child_row(node, 0, path, index)).ok();
                let den = self.el("span", "mn-frac-den");
                den.append_child(&self.child_row(node, 1, path, index)).ok();
                frac.append_child(&num).ok();
                match between {
                    Between::Rule => frac.append_child(&self.el("span", "mn-frac-rule")).ok(),
                    Between::Arrow(arrow) => frac.append_child(&self.arrow(*arrow)).ok(),
                    Between::Nothing => None,
                };
                frac.append_child(&den).ok();
                frac
            }
            Node::Sqrt { index: root, .. } => {
                let sqrt = self.el("span", "mn-sqrt");
                let body_slot = if root.is_some() { 1 } else { 0 };
                if root.is_some() {
                    let degree = self.el("span", "mn-sqrt-index");
                    degree
                        .append_child(&self.child_row(node, 0, path, index))
                        .ok();
                    sqrt.append_child(&degree).ok();
                }
                // The sign is laid over the body, which leaves the body in the
                // text: the root sits on the same baseline as what is around it.
                sqrt.append_child(&self.svg(
                    "mn-radical",
                    "0 0 20 100",
                    "M0 62 L5 62 L11 97 L19 4",
                ))
                .ok();
                let body = self.el("span", "mn-sqrt-body");
                body.append_child(&self.child_row(node, body_slot, path, index))
                    .ok();
                sqrt.append_child(&body).ok();
                sqrt
            }
            Node::Sup(_) => {
                let sup = self.el("span", "mn-sup");
                sup.append_child(&self.child_row(node, 0, path, index)).ok();
                sup
            }
            Node::Sub(_) => {
                let sub = self.el("span", "mn-sub");
                sub.append_child(&self.child_row(node, 0, path, index)).ok();
                sub
            }
            Node::Group { delim, .. } => {
                let group = self.el("span", "mn-group");
                group.append_child(&self.delimiter(delim, true)).ok();
                let body = self.el("span", "mn-group-body");
                body.append_child(&self.child_row(node, 0, path, index))
                    .ok();
                group.append_child(&body).ok();
                group.append_child(&self.delimiter(delim, false)).ok();
                group
            }
            Node::Limits { sym, lower, upper } => {
                let glyph = sym.as_str();
                let container = self.el("span", "mn-bigop mn-bigop-stacked");
                let upper_el = self.el("span", "mn-limit mn-limit-upper");
                upper_el
                    .append_child(&self.child_row(node, 1, path, index))
                    .ok();
                let lower_el = self.el("span", "mn-limit mn-limit-lower");
                lower_el
                    .append_child(&self.child_row(node, 0, path, index))
                    .ok();
                if upper.is_empty() {
                    upper_el.class_list().add_1("mn-limit-empty").ok();
                }
                if lower.is_empty() {
                    lower_el.class_list().add_1("mn-limit-empty").ok();
                }
                let symbol_class = if glyph.chars().count() > 1 {
                    "mn-bigop-symbol mn-bigop-word"
                } else {
                    "mn-bigop-symbol"
                };
                container.append_child(&upper_el).ok();
                container.append_child(&self.span(symbol_class, glyph)).ok();
                container.append_child(&lower_el).ok();
                container
            }
            Node::Matrix { kind, cells } => {
                let container = self.el("span", "mn-matrix");
                let delim = match kind {
                    MatrixKind::Grid => Delim::Bracket,
                    MatrixKind::Cases => Delim::Brace,
                };
                container.append_child(&self.delimiter(&delim, true)).ok();
                let grid = self.el("span", "mn-matrix-grid");
                let cols = cells.first().map(|r| r.len()).unwrap_or(1).max(1);
                grid.set_attribute(
                    "style",
                    &format!("grid-template-columns: repeat({cols}, auto);"),
                )
                .ok();
                for (row_index, row) in cells.iter().enumerate() {
                    for (col_index, _) in row.iter().enumerate() {
                        let cell = self.el("span", "mn-matrix-cell");
                        cell.append_child(&self.child_row(
                            node,
                            row_index * cols + col_index,
                            path,
                            index,
                        ))
                        .ok();
                        grid.append_child(&cell).ok();
                    }
                }
                container.append_child(&grid).ok();
                if matches!(kind, MatrixKind::Grid) {
                    container.append_child(&self.delimiter(&delim, false)).ok();
                }
                container
            }
        }
    }

    /// An arrow between the two rows of a stack. The shaft is a flexible line
    /// so the arrow ends up as wide as the wider row, like the rule is.
    fn arrow(&self, arrow: char) -> Element {
        let holder = self.el("span", "mn-arrow");
        let shaft = || self.el("span", "mn-arrow-shaft");
        let glyph = self.span("mn-arrow-head", &arrow.to_string());
        match arrow {
            // The glyph carries the head, so the line goes on its blunt side.
            '→' | '⇒' | '↦' => {
                holder.append_child(&shaft()).ok();
                holder.append_child(&glyph).ok();
            }
            '←' | '⇐' => {
                holder.append_child(&glyph).ok();
                holder.append_child(&shaft()).ok();
            }
            // Two heads: each end is a glyph of its own, with the line between.
            '↔' | '⇔' => {
                let (left, right) = if arrow == '↔' {
                    ('←', '→')
                } else {
                    ('⇐', '⇒')
                };
                holder
                    .append_child(&self.span("mn-arrow-head", &left.to_string()))
                    .ok();
                holder.append_child(&shaft()).ok();
                holder
                    .append_child(&self.span("mn-arrow-head", &right.to_string()))
                    .ok();
            }
            // A pair of arrows, one above the other, both stretched.
            '⇄' => {
                let column = self.el("span", "mn-arrow-pair");
                column.append_child(&self.arrow('→')).ok();
                column.append_child(&self.arrow('←')).ok();
                return column;
            }
            _ => {
                holder.append_child(&glyph).ok();
            }
        }
        holder
    }

    fn delimiter(&self, delim: &Delim, open: bool) -> Element {
        let (view_box, path) = match (delim, open) {
            (Delim::Paren, true) => ("0 0 12 100", "M9 2 C3 26 3 74 9 98"),
            (Delim::Paren, false) => ("0 0 12 100", "M3 2 C9 26 9 74 3 98"),
            (Delim::Bracket, true) => ("0 0 12 100", "M9 2 L3 2 L3 98 L9 98"),
            (Delim::Bracket, false) => ("0 0 12 100", "M3 2 L9 2 L9 98 L3 98"),
            (Delim::Brace, true) => ("0 0 14 100", "M11 2 C6 6 8 40 3 50 C8 60 6 94 11 98"),
            (Delim::Brace, false) => ("0 0 14 100", "M3 2 C8 6 6 40 11 50 C6 60 8 94 3 98"),
            (Delim::Bar, _) => ("0 0 8 100", "M4 2 L4 98"),
        };
        let side = if open {
            "mn-delim-open"
        } else {
            "mn-delim-close"
        };
        self.svg(&format!("mn-delim {side}"), view_box, path)
    }

    fn el(&self, tag: &str, class: &str) -> Element {
        let el = self.doc.create_element(tag).expect("create element");
        el.set_class_name(class);
        el
    }

    fn span(&self, class: &str, text: &str) -> Element {
        let el = self.el("span", class);
        el.set_text_content(Some(text));
        el
    }

    fn svg(&self, class: &str, view_box: &str, path: &str) -> Element {
        let svg = self
            .doc
            .create_element_ns(Some(SVG_NS), "svg")
            .expect("create svg");
        svg.set_attribute("class", class).ok();
        svg.set_attribute("viewBox", view_box).ok();
        svg.set_attribute("preserveAspectRatio", "none").ok();
        let shape = self
            .doc
            .create_element_ns(Some(SVG_NS), "path")
            .expect("create path");
        shape.set_attribute("d", path).ok();
        shape.set_attribute("fill", "none").ok();
        shape.set_attribute("stroke", "currentColor").ok();
        shape.set_attribute("stroke-width", "1.4").ok();
        shape.set_attribute("stroke-linecap", "round").ok();
        shape
            .set_attribute("vector-effect", "non-scaling-stroke")
            .ok();
        svg.append_child(&shape).ok();
        svg
    }
}

/// How a character is set. Letters read the same as in the text around them;
/// the classes are for spacing — an operator gets air, a digit does not.
fn char_class(c: char, math: bool) -> &'static str {
    if !math {
        return RUN_CLASS;
    }
    if c.is_ascii_digit() {
        "mn-atom mn-num"
    } else if is_variable_letter(c) {
        "mn-atom mn-ident"
    } else if c.is_alphabetic() {
        "mn-atom mn-word"
    } else if matches!(c, '+' | '-' | '=' | '<' | '>' | '±') {
        "mn-atom mn-bin"
    } else {
        "mn-atom mn-punct"
    }
}

fn is_variable_letter(c: char) -> bool {
    c.is_ascii_alphabetic() || matches!(c, 'α'..='ω' | 'Α'..='Ω')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_roundtrip() {
        let path = vec![(0, 1), (2, 0)];
        assert_eq!(decode_path(&encode_path(&path)), Some(path));
    }

    #[test]
    fn the_path_of_a_line_is_empty() {
        assert_eq!(encode_path(&[]), "");
        assert_eq!(decode_path(""), Some(Vec::new()));
    }

    #[test]
    fn prose_is_not_set_as_a_formula() {
        assert_eq!(char_class('a', false), RUN_CLASS);
        assert_eq!(char_class('a', true), "mn-atom mn-ident");
        assert_eq!(char_class('あ', true), "mn-atom mn-word");
    }
}
