//! Draws a formula as DOM elements. Layout is done with flexbox and a few
//! stretchable SVG shapes, so no font or typesetting library is needed.

use wasm_bindgen::JsCast;
use web_sys::{Document, Element};

use super::ast::{Between, Cursor, Delim, MatrixKind, Node, Row};
use super::symbols::{self, Class};

const SVG_NS: &str = "http://www.w3.org/2000/svg";

/// Where the caret would land if the user clicked an element: the row it lives
/// in, and its offset in that row.
pub fn encode_position(path: &[(usize, usize)], index: usize) -> String {
    let path = path
        .iter()
        .map(|(node, slot)| format!("{node}.{slot}"))
        .collect::<Vec<_>>()
        .join(",");
    format!("{path}|{index}")
}

pub fn decode_position(encoded: &str) -> Option<Cursor> {
    let (path, index) = encoded.split_once('|')?;
    let path = path
        .split(',')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let (node, slot) = part.split_once('.')?;
            Some((node.parse().ok()?, slot.parse().ok()?))
        })
        .collect::<Option<Vec<_>>>()?;
    Some(Cursor {
        path,
        index: index.parse().ok()?,
    })
}

pub struct Renderer<'a> {
    doc: &'a Document,
    cursor: Option<&'a Cursor>,
}

impl<'a> Renderer<'a> {
    pub fn new(doc: &'a Document, cursor: Option<&'a Cursor>) -> Renderer<'a> {
        Renderer { doc, cursor }
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

    fn caret(&self) -> Element {
        self.el("span", "mn-caret")
    }

    /// Renders one row; `path` is the address of the row inside the formula.
    pub fn row(&self, row: &Row, path: &[(usize, usize)]) -> Element {
        let container = self.el("span", "mn-row");
        let caret_here = self
            .cursor
            .filter(|cursor| cursor.path == path)
            .map(|cursor| cursor.index);
        if row.is_empty() {
            // Keep empty slots visible and clickable.
            let placeholder = self.el("span", "mn-placeholder");
            placeholder
                .set_attribute("data-pos", &encode_position(path, 0))
                .ok();
            container.append_child(&placeholder).ok();
        }
        for (index, node) in row.iter().enumerate() {
            if caret_here == Some(index) {
                container.append_child(&self.caret()).ok();
            }
            let element = self.node(node, path, index);
            element
                .set_attribute("data-pos", &encode_position(path, index))
                .ok();
            container.append_child(&element).ok();
        }
        if caret_here == Some(row.len()) {
            container.append_child(&self.caret()).ok();
        }
        container
    }

    fn child_row(
        &self,
        node: &Node,
        slot: usize,
        path: &[(usize, usize)],
        index: usize,
    ) -> Element {
        let mut child_path = path.to_vec();
        child_path.push((index, slot));
        let row = node.slot(slot).cloned().unwrap_or_default();
        self.row(&row, &child_path)
    }

    fn node(&self, node: &Node, path: &[(usize, usize)], index: usize) -> Element {
        match node {
            Node::Char(c) => {
                let class = if c.is_ascii_digit() {
                    "mn-atom mn-num"
                } else if c.is_alphabetic() {
                    "mn-atom mn-ident"
                } else if matches!(c, '+' | '-' | '=' | '<' | '>' | '±') {
                    "mn-atom mn-bin"
                } else {
                    "mn-atom mn-punct"
                };
                self.span(class, &c.to_string())
            }
            Node::Sym(name) => {
                let symbol = symbols::lookup(name);
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
                let frac = self.el(
                    "span",
                    match between {
                        Between::Rule => "mn-frac",
                        _ => "mn-frac mn-frac-bare",
                    },
                );
                let num = self.el("span", "mn-frac-num");
                num.append_child(&self.child_row(node, 0, path, index)).ok();
                let den = self.el("span", "mn-frac-den");
                den.append_child(&self.child_row(node, 1, path, index)).ok();
                frac.append_child(&num).ok();
                if let Between::Arrow(arrow) = between {
                    frac.append_child(&self.arrow(*arrow)).ok();
                }
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
                match kind {
                    MatrixKind::Grid => {
                        container
                            .append_child(&self.delimiter(&Delim::Bracket, true))
                            .ok();
                    }
                    MatrixKind::Cases => {
                        container
                            .append_child(&self.delimiter(&Delim::Brace, true))
                            .ok();
                    }
                }
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
                    container
                        .append_child(&self.delimiter(&Delim::Bracket, false))
                        .ok();
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
        self.svg("mn-delim", view_box, path)
    }
}

/// Replaces the contents of `host` with a freshly rendered formula.
pub fn render_into(host: &Element, row: &Row, cursor: Option<&Cursor>) {
    let Some(doc) = host.owner_document() else {
        return;
    };
    host.set_inner_html("");
    let renderer = Renderer::new(&doc, cursor);
    let rendered = renderer.row(row, &[]);
    host.append_child(&rendered).ok();
}

/// Finds the caret position closest to a click inside a rendered formula.
pub fn position_at_point(host: &Element, x: f64, y: f64) -> Option<Cursor> {
    let candidates = host.query_selector_all("[data-pos]").ok()?;
    let mut best: Option<(f64, Cursor)> = None;
    // A click inside a nested slot also lands inside the rectangle of every
    // enclosing structure, so the innermost hit wins over its ancestors.
    let mut inside: Option<(usize, f64, Cursor)> = None;
    for i in 0..candidates.length() {
        let Some(element) = candidates
            .item(i)
            .and_then(|n| n.dyn_into::<Element>().ok())
        else {
            continue;
        };
        let encoded = element.get_attribute("data-pos")?;
        let Some(mut cursor) = decode_position(&encoded) else {
            continue;
        };
        let rect = element.get_bounding_client_rect();
        // Vertical distance dominates so clicking a denominator does not land
        // in the numerator.
        let dy = if y < rect.top() {
            rect.top() - y
        } else if y > rect.bottom() {
            y - rect.bottom()
        } else {
            0.0
        };
        let middle = rect.left() + rect.width() / 2.0;
        let dx = (x - middle).abs();
        if x > middle {
            cursor.index += 1;
        }
        let score = dy * 4.0 + dx;
        if dy == 0.0 && x >= rect.left() && x <= rect.right() {
            let depth = cursor.path.len();
            let better = inside
                .as_ref()
                .is_none_or(|(deep, close, _)| depth > *deep || (depth == *deep && score < *close));
            if better {
                inside = Some((depth, score, cursor));
            }
            continue;
        }
        if best.as_ref().is_none_or(|(current, _)| score < *current) {
            best = Some((score, cursor));
        }
    }
    inside
        .map(|(_, _, cursor)| cursor)
        .or(best.map(|(_, cursor)| cursor))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_roundtrip() {
        let cursor = Cursor {
            path: vec![(0, 1), (2, 0)],
            index: 3,
        };
        let encoded = encode_position(&cursor.path, cursor.index);
        assert_eq!(decode_position(&encoded), Some(cursor));
    }

    #[test]
    fn root_positions_roundtrip() {
        let encoded = encode_position(&[], 0);
        assert_eq!(decode_position(&encoded), Some(Cursor::root(0)));
    }
}
