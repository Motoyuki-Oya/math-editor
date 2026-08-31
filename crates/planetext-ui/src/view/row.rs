//! 行を描画する 1 つのコンポーネントです。
//!
//! 行は、ドキュメントの行、アイランドの内部、分数の分子など、どこにあっても行です。これらはすべてこのコンポーネントによって描画されるため、文字、IME のコミットされていないテキスト、およびネストされた構造は、どの深さでも同じように動作します。行はパスからその行がどこにあるかを認識しており、それが唯一の違いです。ドキュメントの行のパスは空で、行は列区切り文字を受け入れる唯一の行です。

use web_sys::{Document, Element};

use crate::settings;
use crate::structure::ast::{Between, Delim, MatrixKind, Node, NodeKind, Row};

const SVG_NS: &str = "http://www.w3.org/2000/svg";
const LIMIT_SCALE: f64 = 0.62;
const SCRIPT_SCALE: f64 = 0.72;
const ROOT_INDEX_SCALE: f64 = 0.7;

pub const ROW_CLASS: &str = "mn-row";
pub const RUN_CLASS: &str = "mn-run";
pub const TAB_CLASS: &str = "mn-tab";
pub const PREEDIT_CLASS: &str = "mn-preedit";
/// 空の行が表示され、クリックできるように表示されるボックス。
pub const PLACEHOLDER_CLASS: &str = "mn-placeholder";
/// 要素が存在する行のパス。各行に書き込まれます。
pub const PATH_ATTR: &str = "data-path";
/// 要素が開始する行内の場所。行内のすべてに書き込まれます。
pub const START_ATTR: &str = "data-start";

/// Where行は、そこに到達するために通過するスロットです。スロットは `(その行内のものの位置、その行のどの位置)` です。したがって、`1.0,2.1` は、その行の 1 行目の 3 番目の 2 番目の行の 2 番目の行です。
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

/// A rendered node. Characters are grouped into runs, while tabs and structures
/// remain individual DOM siblings.
pub enum Cell<'a> {
    Char(char),
    Space,
    ZenkakuSpace,
    Tab,
    Node(&'a Node),
}

pub fn cells_of_row(row: &[Node]) -> Vec<Cell<'_>> {
    row.iter()
        .map(|node| match &node.kind {
            NodeKind::Char(' ') => Cell::Space,
            NodeKind::Char('\u{3000}') => Cell::ZenkakuSpace,
            NodeKind::Char(c) => Cell::Char(*c),
            NodeKind::Tab => Cell::Tab,
            _ => Cell::Node(node),
        })
        .collect()
}

/// IME がまだコミットしていないテキスト、およびドキュメント内の配置先: 行とそのどこまで。
pub struct Preedit<'a> {
    pub path: Vec<(usize, usize)>,
    pub index: usize,
    pub text: &'a str,
}

#[derive(Clone, Copy)]
struct VerticalMetrics {
    above: f64,
    below: f64,
}

impl VerticalMetrics {
    fn height(self) -> f64 {
        self.above + self.below
    }
}

pub struct Renderer<'a> {
    doc: &'a Document,
    preedit: Option<&'a Preedit<'a>>,
    active: Option<&'a Path>,
    font_size: f64,
    language: Option<&'a crate::syntax::lang::LanguageDef>,
    ghost_text: Option<&'a crate::syntax::GhostText>,
}

/// 待機中の文字1 つのスパンとして描画されます。これにより、テキストの間隔と形状が維持されます。
struct Run {
    start: usize,
    class: &'static str,
    text: String,
    kind: Option<crate::syntax::TokenKind>,
}

fn run_class_for(kind: Option<crate::syntax::TokenKind>) -> &'static str {
    match kind {
        None => RUN_CLASS,
        Some(crate::syntax::TokenKind::Keyword) => "mn-run mn-syn-keyword",
        Some(crate::syntax::TokenKind::Type) => "mn-run mn-syn-type",
        Some(crate::syntax::TokenKind::String) => "mn-run mn-syn-string",
        Some(crate::syntax::TokenKind::Comment) => "mn-run mn-syn-comment",
        Some(crate::syntax::TokenKind::Builtin) => "mn-run mn-syn-builtin",
        Some(crate::syntax::TokenKind::Constant) => "mn-run mn-syn-constant",
        Some(crate::syntax::TokenKind::Operator) => "mn-run mn-syn-operator",
        Some(crate::syntax::TokenKind::Punctuation) => "mn-run mn-syn-punct",
    }
}

fn cells_to_plain(cells: &[Cell<'_>]) -> String {
    let mut s = String::with_capacity(cells.len());
    for cell in cells {
        match cell {
            Cell::Char(c) => s.push(*c),
            Cell::Space => s.push(' '),
            Cell::ZenkakuSpace => s.push('\u{3000}'),
            Cell::Tab => s.push('\t'),
            Cell::Node(_) => s.push('\u{fffc}'),
        }
    }
    s
}

fn token_kind_at(
    spans: &[crate::syntax::TokenSpan],
    index: usize,
) -> Option<crate::syntax::TokenKind> {
    for span in spans {
        if index < span.start {
            break;
        }
        if index < span.end {
            return Some(span.kind);
        }
    }
    None
}

fn token_spans(
    cells: &[Cell<'_>],
    language: Option<&crate::syntax::lang::LanguageDef>,
    nested: bool,
) -> Vec<crate::syntax::TokenSpan> {
    if nested {
        return Vec::new();
    }
    language
        .map(|lang| crate::syntax::tokenize_line(&cells_to_plain(cells), lang))
        .unwrap_or_default()
}

impl<'a> Renderer<'a> {
    pub fn new(doc: &'a Document) -> Renderer<'a> {
        Renderer {
            doc,
            preedit: None,
            active: None,
            font_size: settings::current().font_size,
            language: None,
            ghost_text: None,
        }
    }

    pub fn with_preedit(mut self, preedit: Option<&'a Preedit<'a>>) -> Renderer<'a> {
        self.preedit = preedit.filter(|preedit| !preedit.text.is_empty());
        self
    }

    pub fn with_active_path(mut self, active: Option<&'a Path>) -> Renderer<'a> {
        self.active = active;
        self
    }

    pub fn with_language(
        mut self,
        language: Option<&'a crate::syntax::lang::LanguageDef>,
    ) -> Renderer<'a> {
        self.language = language;
        self
    }

    pub fn with_ghost_text(
        mut self,
        ghost_text: Option<&'a crate::syntax::GhostText>,
    ) -> Renderer<'a> {
        self.ghost_text = ghost_text;
        self
    }

    /// ドキュメントの線を描画します。行とは、パスが空の行です。
    pub fn line(&self, row: &[Node]) -> Element {
        self.row(&cells_of_row(row), &[], self.font_size)
    }

    /// どこにでも 1 行を描画します。
    pub fn row(&self, cells: &[Cell<'_>], path: &Path, font_size: f64) -> Element {
        // 入れ子の空Rowだけは、編集位置を示すプレースホルダーが必要です。
        let nested = !path.is_empty();
        let container = self.el("span", ROW_CLASS);
        container.set_attribute(PATH_ATTR, &encode_path(path)).ok();
        if cells.is_empty() {
            container.append_child(&self.empty(nested)).ok();
        }

        let token_spans = token_spans(cells, self.language, nested);

        let is_table_row = path.is_empty()
            && self.language.is_some_and(|l| l.name == "Markdown")
            && is_markdown_table_row(cells);

        let mut run: Option<Run> = None;
        for (index, cell) in cells.iter().enumerate() {
            if let Some(preedit) = self.preedit_at(path, index) {
                self.flush(&container, &mut run);
                container.append_child(&preedit).ok();
            }

            if let Some(ghost) = self.ghost_text {
                if path.is_empty() && !ghost.suffix.is_empty() && index == ghost.col {
                    self.flush(&container, &mut run);
                    let ghost_el = self.span("mn-ghost-text", &ghost.suffix);
                    container.append_child(&ghost_el).ok();
                }
            }

            let kind = token_kind_at(&token_spans, index);

            match cell {
                Cell::Char('|') if is_table_row => {
                    self.flush(&container, &mut run);
                    let element = self.span("mn-table-pipe", "|");
                    element.set_attribute(START_ATTR, &index.to_string()).ok();
                    container.append_child(&element).ok();
                }
                Cell::Char(c) => {
                    if let Some(r) = run.as_mut() {
                        if r.kind == kind {
                            r.text.push(*c);
                        } else {
                            self.flush(&container, &mut run);
                            run = Some(Run {
                                start: index,
                                class: run_class_for(kind),
                                text: c.to_string(),
                                kind,
                            });
                        }
                    } else {
                        run = Some(Run {
                            start: index,
                            class: run_class_for(kind),
                            text: c.to_string(),
                            kind,
                        });
                    }
                }
                cell => {
                    self.flush(&container, &mut run);
                    let element = self.cell(cell, path, index, font_size, kind);
                    element.set_attribute(START_ATTR, &index.to_string()).ok();
                    container.append_child(&element).ok();
                }
            }
        }
        self.flush(&container, &mut run);
        if let Some(preedit) = self.preedit_at(path, cells.len()) {
            container.append_child(&preedit).ok();
        }
        if let Some(ghost) = self.ghost_text {
            if path.is_empty() && !ghost.suffix.is_empty() && ghost.col >= cells.len() {
                let ghost_el = self.span("mn-ghost-text", &ghost.suffix);
                container.append_child(&ghost_el).ok();
            }
        }
        container
    }

    /// 空の行でも、測定してクリックする必要があります。
    fn empty(&self, nested: bool) -> Element {
        let class = if nested { PLACEHOLDER_CLASS } else { RUN_CLASS };
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
        let run_chars_count = run.text.chars().count();
        if let Some(ghost) = self.ghost_text {
            if let Some((kanji_len, ref reading)) = ghost.ruby {
                let kanji_start = ghost.col.saturating_sub(kanji_len);
                if ghost.col == run.start + run_chars_count && kanji_start >= run.start {
                    let prefix_chars = kanji_start - run.start;
                    if prefix_chars > 0 {
                        let prefix_text: String = run.text.chars().take(prefix_chars).collect();
                        let prefix_el = self.span(run.class, &prefix_text);
                        prefix_el
                            .set_attribute(START_ATTR, &run.start.to_string())
                            .ok();
                        container.append_child(&prefix_el).ok();
                    }
                    let kanji_text: String = run.text.chars().skip(prefix_chars).collect();
                    let target_class = format!("{} mn-ghost-ruby-target", run.class);
                    let kanji_el = self.span(&target_class, &kanji_text);
                    kanji_el
                        .set_attribute(START_ATTR, &kanji_start.to_string())
                        .ok();
                    let ruby_el = self.span("mn-ghost-ruby", reading);
                    kanji_el.append_child(&ruby_el).ok();
                    container.append_child(&kanji_el).ok();
                    return;
                }
            }
        }
        let element = self.span(run.class, &run.text);
        element
            .set_attribute(START_ATTR, &run.start.to_string())
            .ok();
        container.append_child(&element).ok();
    }

    fn cell(
        &self,
        cell: &Cell<'_>,
        path: &Path,
        index: usize,
        font_size: f64,
        kind: Option<crate::syntax::TokenKind>,
    ) -> Element {
        match cell {
            // 上記の実行によって処理されます。ランは決して独自のセルではありません。
            Cell::Char(c) => self.span(run_class_for(kind), &c.to_string()),
            Cell::Space => self.span("mn-run mn-space", " "),
            Cell::ZenkakuSpace => self.span("mn-run mn-zenkaku", "\u{3000}"),
            Cell::Tab => self.el("span", TAB_CLASS),
            Cell::Node(node) => self.node(node, path, index, font_size, kind),
        }
    }

    fn child_row(
        &self,
        node: &Node,
        slot: usize,
        path: &Path,
        index: usize,
        font_size: f64,
    ) -> Element {
        let mut child = path.to_vec();
        child.push((index, slot));
        let row = node.slot(slot).cloned().unwrap_or_default();
        self.row(&cells_of_row(&row), &child, font_size)
    }

    fn node(
        &self,
        node: &Node,
        path: &Path,
        index: usize,
        font_size: f64,
        kind: Option<crate::syntax::TokenKind>,
    ) -> Element {
        let base = match &node.kind {
            NodeKind::Char(c) => self.span(run_class_for(kind), &c.to_string()),
            NodeKind::Tab => self.el("span", TAB_CLASS),
            NodeKind::Stack { above, between, .. } => {
                let frac = self.el("span", "mn-frac");
                let shift = stack_axis_shift(above, between, font_size);
                frac.set_attribute("style", &format!("vertical-align:{shift}px"))
                    .ok();
                let num = self.el("span", "mn-frac-num");
                num.append_child(&self.child_row(node, 0, path, index, font_size))
                    .ok();
                let den = self.el("span", "mn-frac-den");
                den.append_child(&self.child_row(node, 1, path, index, font_size))
                    .ok();
                frac.append_child(&num).ok();
                match between {
                    Between::Rule => frac
                        .append_child(&self.el("span", "mn-frac-rule"))
                        .ok(),
                    Between::Arrow(arrow) => frac.append_child(&self.arrow(*arrow)).ok(),
                    Between::Nothing => None,
                };
                frac.append_child(&den).ok();
                frac
            }
            NodeKind::Sqrt { index: root, .. } => {
                let sqrt = self.el("span", "mn-sqrt");
                let body_slot = if root.is_some() { 1 } else { 0 };
                if root.is_some() {
                    let degree = self.el("span", "mn-sqrt-index");
                    degree
                        .append_child(&self.child_row(
                            node,
                            0,
                            path,
                            index,
                            font_size * ROOT_INDEX_SCALE,
                        ))
                        .ok();
                    sqrt.append_child(&degree).ok();
                }
                // 記号は本体の上に配置され、テキスト内に本体が残ります。つまり、ルートはその周囲にあるものと同じベースライン上にあります。キック、下降、および長い上昇ストローク。直立ではなく傾斜しています。この傾斜により、記号は括弧ではなく部首として読み取られます。
                sqrt.append_child(&self.radical()).ok();
                let body = self.el("span", "mn-sqrt-body");
                body.append_child(&self.child_row(node, body_slot, path, index, font_size))
                    .ok();
                sqrt.append_child(&body).ok();
                sqrt
            }
            NodeKind::Sup(_) => {
                let sup = self.el("span", "mn-sup");
                sup.append_child(&self.child_row(node, 0, path, index, font_size * SCRIPT_SCALE))
                    .ok();
                sup
            }
            NodeKind::Sub(_) => {
                let sub = self.el("span", "mn-sub");
                sub.append_child(&self.child_row(node, 0, path, index, font_size * SCRIPT_SCALE))
                    .ok();
                sub
            }
            NodeKind::Group { delim, .. } => {
                let group = self.el("span", "mn-group");
                group.append_child(&self.delimiter(delim, true)).ok();
                let body = self.el("span", "mn-group-body");
                body.append_child(&self.child_row(node, 0, path, index, font_size))
                    .ok();
                group.append_child(&body).ok();
                group.append_child(&self.delimiter(delim, false)).ok();
                group
            }
            NodeKind::BigOp(glyph) => {
                let class = if glyph.chars().count() > 1 {
                    "mn-bigop-symbol mn-bigop-word"
                } else {
                    "mn-bigop-symbol"
                };
                self.span(class, glyph)
            }
            NodeKind::Container(_) => self.child_row(node, 0, path, index, font_size),
            NodeKind::Matrix { kind, cells } => {
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
                            font_size,
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
        };

        let lower_slot = node.lower_slot();
        let upper_slot = node.upper_slot();
        let is_active = |slot| {
            let mut child = path.to_vec();
            child.push((index, slot));
            self.active.is_some_and(|active| active == child.as_slice())
        };
        let show_upper = !node.upper.is_empty() || is_active(upper_slot);
        let show_lower = !node.lower.is_empty() || is_active(lower_slot);
        if !show_upper && !show_lower {
            if matches!(&node.kind, NodeKind::BigOp(_)) {
                base.class_list().add_1("mn-bigop").ok();
            }
            self.apply_token_class(&base, kind);
            return base;
        }
        if matches!(&node.kind, NodeKind::BigOp(_)) {
            let container = self.el("span", "mn-bigop mn-bigop-stacked");
            if show_upper {
                let upper = self.el("span", "mn-limit mn-limit-upper");
                upper
                    .append_child(&self.child_row(
                        node,
                        upper_slot,
                        path,
                        index,
                        font_size * LIMIT_SCALE,
                    ))
                    .ok();
                container.append_child(&upper).ok();
            }
            container.append_child(&base).ok();
            if show_lower {
                let lower = self.el("span", "mn-limit mn-limit-lower");
                lower
                    .append_child(&self.child_row(
                        node,
                        lower_slot,
                        path,
                        index,
                        font_size * LIMIT_SCALE,
                    ))
                    .ok();
                container.append_child(&lower).ok();
            }
            self.apply_token_class(&container, kind);
            return container;
        }
        let container = self.el("span", "mn-annotated");
        if show_upper {
            let upper = self.el("span", "mn-limit mn-limit-upper");
            upper
                .append_child(&self.child_row(
                    node,
                    upper_slot,
                    path,
                    index,
                    font_size * LIMIT_SCALE,
                ))
                .ok();
            container.append_child(&upper).ok();
        }
        container.append_child(&base).ok();
        if show_lower {
            let lower = self.el("span", "mn-limit mn-limit-lower");
            lower
                .append_child(&self.child_row(
                    node,
                    lower_slot,
                    path,
                    index,
                    font_size * LIMIT_SCALE,
                ))
                .ok();
            container.append_child(&lower).ok();
        }
        self.apply_token_class(&container, kind);
        container
    }

    fn apply_token_class(&self, element: &Element, kind: Option<crate::syntax::TokenKind>) {
        let Some(kind) = kind else { return };
        let class = match kind {
            crate::syntax::TokenKind::Keyword => "mn-syn-keyword",
            crate::syntax::TokenKind::Type => "mn-syn-type",
            crate::syntax::TokenKind::String => "mn-syn-string",
            crate::syntax::TokenKind::Comment => "mn-syn-comment",
            crate::syntax::TokenKind::Builtin => "mn-syn-builtin",
            crate::syntax::TokenKind::Constant => "mn-syn-constant",
            crate::syntax::TokenKind::Operator => "mn-syn-operator",
            crate::syntax::TokenKind::Punctuation => "mn-syn-punct",
        };
        element.class_list().add_1(class).ok();
    }

    /// スタックの 2 行間の矢印。シャフトは柔軟な線なので、ルールと同様に、矢印は幅の広い行と同じ幅になります。
    fn arrow(&self, arrow: char) -> Element {
        let holder = self.el("span", "mn-arrow");
        let shaft = || self.el("span", "mn-arrow-shaft");
        let glyph = self.span("mn-arrow-head", &arrow.to_string());
        match arrow {
            // グリフは頭を運ぶので、線は鈍い側に進みます。
            '→' | '⇒' | '↦' => {
                holder.append_child(&shaft()).ok();
                holder.append_child(&glyph).ok();
            }
            '←' | '⇐' => {
                holder.append_child(&glyph).ok();
                holder.append_child(&shaft()).ok();
            }
            // 2 つの頭: それぞれの端は独自のグリフで、間に線があります。
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
            // 一対の矢印が上下に並び、両方とも引き伸ばされます。
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

    fn radical(&self) -> Element {
        let svg = self
            .doc
            .create_element_ns(Some(SVG_NS), "svg")
            .expect("create radical");
        svg.set_attribute("class", "mn-radical").ok();
        for (x1, y1, x2, y2) in [
            ("0", "58%", "0.16em", "60%"),
            ("0.16em", "60%", "0.38em", "96%"),
            ("0.38em", "96%", "0.82em", "0"),
            ("0.82em", "0", "100%", "0"),
        ] {
            let line = self
                .doc
                .create_element_ns(Some(SVG_NS), "line")
                .expect("create radical line");
            for (name, value) in [("x1", x1), ("y1", y1), ("x2", x2), ("y2", y2)] {
                line.set_attribute(name, value).ok();
            }
            line.set_attribute("stroke", "currentColor").ok();
            line.set_attribute("stroke-width", "1").ok();
            line.set_attribute("stroke-linecap", "round").ok();
            line.set_attribute("vector-effect", "non-scaling-stroke").ok();
            svg.append_child(&line).ok();
        }
        svg
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
        shape.set_attribute("stroke-linejoin", "round").ok();
        svg.append_child(&shape).ok();
        svg
    }
}

fn stack_axis_shift(above: &Row, between: &Between, font_size: f64) -> f64 {
    row_vertical_metrics(above, font_size).below
        + font_size * 0.22
        + separator_height(between, font_size) / 2.0
}

fn separator_height(between: &Between, font_size: f64) -> f64 {
    match between {
        Between::Rule => 1.0,
        Between::Arrow(_) => font_size,
        Between::Nothing => 0.0,
    }
}

fn row_vertical_metrics(row: &Row, font_size: f64) -> VerticalMetrics {
    row.iter()
        .fold(text_vertical_metrics(font_size), |row, node| {
            let node = node_vertical_metrics(node, font_size);
            VerticalMetrics {
                above: row.above.max(node.above),
                below: row.below.max(node.below),
            }
        })
}

fn node_vertical_metrics(node: &Node, font_size: f64) -> VerticalMetrics {
    let mut metrics = match &node.kind {
        NodeKind::Char(_) | NodeKind::Tab => text_vertical_metrics(font_size),
        NodeKind::BigOp(_) => VerticalMetrics {
            above: font_size * 1.05,
            below: font_size * 0.45,
        },
        NodeKind::Stack {
            above,
            below,
            between,
        } => {
            let above = row_vertical_metrics(above, font_size);
            let below = row_vertical_metrics(below, font_size);
            let separator = separator_height(between, font_size);
            VerticalMetrics {
                above: above.height() + font_size * 0.22 + separator / 2.0,
                below: below.height() + font_size * 0.26 + separator / 2.0,
            }
        }
        NodeKind::Sqrt { index, body } => {
            let body = row_vertical_metrics(body, font_size);
            let index = index
                .as_ref()
                .map(|row| row_vertical_metrics(row, font_size * ROOT_INDEX_SCALE).height())
                .unwrap_or(0.0);
            VerticalMetrics {
                above: (body.above + font_size * 0.2).max(index),
                below: body.below + font_size * 0.08,
            }
        }
        NodeKind::Sup(row) => {
            let row = row_vertical_metrics(row, font_size * SCRIPT_SCALE);
            VerticalMetrics {
                above: row.height() + font_size * 0.35,
                below: font_size * 0.1,
            }
        }
        NodeKind::Sub(row) => {
            let row = row_vertical_metrics(row, font_size * SCRIPT_SCALE);
            VerticalMetrics {
                above: font_size * 0.7,
                below: row.height(),
            }
        }
        NodeKind::Group { body, .. } | NodeKind::Container(body) => {
            row_vertical_metrics(body, font_size)
        }
        NodeKind::Matrix { cells, .. } => {
            let rows: Vec<f64> = cells
                .iter()
                .map(|row| {
                    row.iter()
                        .map(|cell| row_vertical_metrics(cell, font_size).height())
                        .fold(font_size * 2.0, f64::max)
                })
                .collect();
            let height =
                rows.iter().sum::<f64>() + font_size * 0.15 * rows.len().saturating_sub(1) as f64;
            VerticalMetrics {
                above: height / 2.0,
                below: height / 2.0,
            }
        }
    };
    if matches!(&node.kind, NodeKind::BigOp(_)) {
        if !node.upper.is_empty() {
            metrics.above += row_vertical_metrics(&node.upper, font_size * LIMIT_SCALE).height();
        }
        if !node.lower.is_empty() {
            metrics.below += row_vertical_metrics(&node.lower, font_size * LIMIT_SCALE).height();
        }
    } else if !node.upper.is_empty() || !node.lower.is_empty() {
        let upper = if node.upper.is_empty() {
            0.0
        } else {
            row_vertical_metrics(&node.upper, font_size * LIMIT_SCALE).height()
        };
        let lower = if node.lower.is_empty() {
            0.0
        } else {
            row_vertical_metrics(&node.lower, font_size * LIMIT_SCALE).height()
        };
        metrics.above += upper.max(font_size * 0.62);
        metrics.below += lower.max(font_size * 0.62);
    }
    metrics
}

fn text_vertical_metrics(font_size: f64) -> VerticalMetrics {
    VerticalMetrics {
        above: font_size * 1.4,
        below: font_size * 0.6,
    }
}

pub fn is_markdown_table_row(cells: &[Cell<'_>]) -> bool {
    let plain = cells_to_plain(cells);
    let trimmed = plain.trim();
    if trimmed.starts_with('#') || trimmed.starts_with("```") || trimmed.starts_with('`') {
        return false;
    }
    trimmed.starts_with('|') && trimmed.chars().filter(|&c| c == '|').count() >= 2
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
    fn fraction_axis_uses_the_recursive_height_below_its_numerator_baseline() {
        let plain = vec![Node::char('a')];
        assert!((stack_axis_shift(&plain, &Between::Rule, 15.0) - 12.8).abs() < 0.001);
        let nested = vec![Node::stack(
            vec![Node::char('a')],
            vec![Node::char('b')],
            Between::Rule,
        )];
        assert!(stack_axis_shift(&nested, &Between::Rule, 15.0) > 12.8);
        let limit_font = 15.0 * LIMIT_SCALE;
        assert!(
            (stack_axis_shift(&plain, &Between::Rule, limit_font) - (limit_font * 0.82 + 0.5))
                .abs()
                < 0.001
        );
        let nested_scale = limit_font * SCRIPT_SCALE;
        assert!(stack_axis_shift(&plain, &Between::Rule, nested_scale) < limit_font);
    }

    #[test]
    fn nested_rows_inherit_the_outer_token_without_being_reparsed() {
        let language = crate::syntax::for_path("sample.rs").expect("Rust language");
        let keyword_row: Row = "let".chars().map(Node::char).collect();
        let keyword_cells = cells_of_row(&keyword_row);
        let outer = token_spans(&keyword_cells, Some(&language), false);
        assert_eq!(outer[0].kind, crate::syntax::TokenKind::Keyword);
        assert!(token_spans(&keyword_cells, Some(&language), true).is_empty());

        let inherited = crate::syntax::TokenSpan {
            start: 0,
            end: 1,
            kind: crate::syntax::TokenKind::Keyword,
        };
        assert_eq!(
            token_kind_at(&[inherited], 0),
            Some(crate::syntax::TokenKind::Keyword)
        );
    }

    #[test]
    fn a_structure_before_plus_does_not_make_plus_a_markdown_list_marker() {
        let language = crate::syntax::for_path("sample.md").expect("Markdown language");
        let row = vec![
            Node::sqrt(None, vec![Node::char('4')]),
            Node::char(' '),
            Node::char('+'),
            Node::char(' '),
            Node::sqrt(None, vec![Node::char('8')]),
        ];
        let cells = cells_of_row(&row);
        assert_eq!(cells_to_plain(&cells), "\u{fffc} + \u{fffc}");
        assert!(token_spans(&cells, Some(&language), false).is_empty());
    }

    #[test]
    fn structure_characters_are_rendered_as_run_cells() {
        let row = vec![Node::char('+'), Node::char('d')];
        let cells = cells_of_row(&row);
        assert!(matches!(
            cells.as_slice(),
            [Cell::Char('+'), Cell::Char('d')]
        ));
    }
}
