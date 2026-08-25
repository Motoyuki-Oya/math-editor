//! ドキュメント、キャレット、選択範囲をページに描画します。
//!
//! すべての行は、行を描画する 1 つのコンポーネントである [`crate::view::row`] によって描画されるため、テキストの行と構造の内部は、ここでのすべてのものと同じです。構造内の行にできない唯一のことは、列セパレータを隣接する行と並べることです。
//!
//! 描画されたものがどこに到達したかは、[`crate::view::measure`] によって読み戻されます。画面上に何かを配置することと、そこにあるものを測定することは反対の仕事であるため、それらは離れたままになります。
//!
//! ここにあるものはブラウザでは編集できません。行は単純なスパンであり、すべてのキャレットは絶対に配置された小さな要素です。キャレットは一度に表示でき、構造内のキャレットがテキスト内のキャレットとどのように同じであるかについて説明します。
//!
//! ページには見えている行だけがあります。どの行を出すか、描かれていない行の場所取り、ビューをどこへ持っていくかは [`crate::view::viewport`] の仕事で、ここは頼まれた行を描くだけです。

use std::cell::{Cell, RefCell};
use std::ops::Range;

use web_sys::{Document, Element, HtmlElement};

use crate::structure::ast::{Cursor, Node, NodeKind};
use crate::structure::text::{Pos, Sel, Text};
use crate::view::measure::{self, Box2, Hit};
use crate::view::row::{self, Path, Preedit, Renderer, PATH_ATTR, TAB_CLASS};
use crate::view::viewport::Viewport;

pub const LINE_CLASS: &str = "mn-line";
pub(super) const LINE_ATTR: &str = "data-line";

/// 入力中の検索に一致した、描画済みの行内の範囲。選択とは独立して描く。
pub struct Highlight {
    pub line: usize,
    pub path: Vec<(usize, usize)>,
    pub from: usize,
    pub to: usize,
}
/// 線の横に表示される数字。ガターだけに配置されるため、テキストの描画、測定、ヒットテストには入りません。
const NUMBER_CLASS: &str = "mn-number";

#[derive(Default, PartialEq, Eq)]
struct RulerCache {
    total_lines: usize,
    modified: Vec<usize>,
    highlights: Vec<usize>,
}

pub struct View {
    /// エディター全体の箱。マウスとホイールを受け取ります。縦にはスクロール
    /// しません。文書はファイルを覗く窓で、ページに置くのは見えている行だけです。
    pub root: HtmlElement,
    /// 中身を横にだけスクロールする要素。入力欄もこの中で行と一緒に動きます。
    scroller: HtmlElement,
    /// 縦のつまみ。数千 px の空間で文書全体のおおよその割合を示します。
    scrollbar: HtmlElement,
    content: Element,
    document: Element,
    gutter: Element,
    overlay: Element,
    ruler: Element,
    /// どの行をページに出すかと、窓をどこへ持っていくか。
    viewport: Viewport,
    /// スクロールバー上の変更・ハイライトマーカーの前回描画状態キャッシュ。
    ruler_cache: RefCell<RulerCache>,
    /// 前回のガター桁数（桁数が変わった時だけCSSスタイル更新を行う）。
    gutter_digits: Cell<usize>,
}

/// キャレットがどこにあるか、描画がそれについて知る必要があるのはそれだけです。テキスト内の場所、キャレットがそこにある構造のどの深さまで到達しているか、IME がそこで何を構成しているかです。モードはありません。同じキャレットで両方のケースを説明します。
pub struct Overlay<'a> {
    pub sels: &'a [Sel],
    pub highlights: &'a [Highlight],
    pub modified: &'a [usize],
    pub primary: &'a Caret<'a>,
    pub carets: &'a [Caret<'a>],
    pub focused: bool,
    pub linked: bool,
}

#[derive(Default)]
pub struct Caret<'a> {
    pub at: Pos,
    /// キャレットが `at` の構造内にあるときに設定されます。
    pub inside: Option<&'a Cursor>,
    /// IME がまだコミットしていないテキスト。着地する場所に描画されます。
    pub composing: Option<&'a str>,
}

impl Caret<'_> {
    /// キャレットが含まれる行と、その行のどこまで入っているか。テキスト内のキャレットはその行の行にあります。構造内のキャレットは、その構造の行内にあり、`at` にあるアイランドを通って到達します。
    fn place(&self) -> (Vec<(usize, usize)>, usize) {
        match self.inside {
            None => (Vec::new(), self.at.col),
            Some(cursor) => (cursor.path.clone(), cursor.index),
        }
    }
}

impl View {
    /// 呼び出し元が所有する `root` 内にレイヤーを構築します。
    pub fn new(root: HtmlElement) -> Option<Self> {
        use wasm_bindgen::JsCast;
        let doc = root.owner_document()?;
        root.set_inner_html("");
        let scroller = element(&doc, "div", "mn-hscroll")?
            .dyn_into::<HtmlElement>()
            .ok()?;
        let content = element(&doc, "div", "mn-content")?;
        let gutter = element(&doc, "div", "mn-gutter")?;
        let document = element(&doc, "div", "mn-document")?;
        let overlay = element(&doc, "div", "mn-overlay")?;
        append(&content, &gutter);
        append(&content, &document);
        append(&content, &overlay);
        append(&scroller, &content);
        append(&root, &scroller);
        let scrollbar = element(&doc, "div", "mn-vscroll")?
            .dyn_into::<HtmlElement>()
            .ok()?;
        let thumb_space = element(&doc, "div", "mn-thumb-space")?;
        append(&scrollbar, &thumb_space);
        append(&root, &scrollbar);
        let ruler = element(&doc, "div", "mn-ruler")?;
        append(&root, &ruler);
        Some(Self {
            viewport: Viewport::new(
                root.clone(),
                document.clone(),
                scrollbar.clone(),
                thumb_space,
            ),
            root,
            scroller,
            scrollbar,
            content,
            document,
            gutter,
            overlay,
            ruler,
            ruler_cache: RefCell::new(RulerCache::default()),
            gutter_digits: Cell::new(0),
        })
    }

    /// 入力欄を置く先。中身と一緒に横へ動く要素。
    pub fn scroller(&self) -> HtmlElement {
        self.scroller.clone()
    }

    /// 縦のつまみの要素。スクロールを聞く先。
    pub fn scrollbar(&self) -> HtmlElement {
        self.scrollbar.clone()
    }

    /// ホイールの分だけ窓を動かします。描き直しは呼び出し側が行います。
    pub fn wheel(&self, pixels: f64) {
        self.viewport.nudge(pixels);
    }

    /// つまみの位置へ窓を合わせます。こちらから合わせた反響なら `false`。
    pub fn follow_thumb(&self) -> bool {
        match self.viewport.thumb_ratio() {
            Some(ratio) => {
                self.viewport.jump_to_ratio(ratio);
                true
            }
            None => false,
        }
    }

    /// いまページに出ている行の範囲。届いた行を描き直すかの判断に使われます。
    pub fn drawn(&self) -> Range<usize> {
        self.viewport.drawn()
    }

    /// 窓の中の行の中身が変わった（届いた）ので、次の描き直しに必ず描かせます。
    pub fn invalidate(&self) {
        self.viewport.invalidate();
    }

    /// 行DOMを作り直さず、検索一致・選択・キャレット・ルーラーの重ねだけを更新する。
    pub fn redraw_overlay(&self, line_count: usize, state: &Overlay<'_>) {
        if let Some(doc) = self.overlay.owner_document() {
            self.draw_overlay(&doc, state);
            self.draw_ruler(&doc, line_count, state);
        }
    }

    /// 変更後に描画し、キャレットの行をページ内に移動します。
    pub fn draw(&self, text: &Text, state: &Overlay<'_>) {
        self.paint(text, state, true);
    }

    /// スクロール後に描画します。これによりビューがキャレットに移動してはなりません。スクロールバーはキャレットではなくユーザーのものです。
    pub fn repaint(&self, text: &Text, state: &Overlay<'_>) {
        self.paint(text, state, false);
    }

    fn paint(&self, text: &Text, state: &Overlay<'_>, follow_caret: bool) {
        let caret = state.primary;
        let wrap = crate::settings::wrap();
        if wrap {
            self.content.class_list().remove_1("mn-nowrap").ok();
        } else {
            self.content.class_list().add_1("mn-nowrap").ok();
        }
        self.fit_numbers(text.line_count());
        // IME が作成しているテキストは、どの行にあってもキャレットに属します。行を描画するコンポーネントがそれをそこに配置します。
        let (path, index) = caret.place();
        let preedit = caret.composing.map(|text| Preedit {
            path: path.clone(),
            index,
            text,
        });
        let draw_line = |doc: &Document, line: usize| {
            // キャレットがある行のみ、構成内容が表示されます。
            let here = preedit
                .as_ref()
                .filter(|_| caret.at.line == line)
                .map(|preedit| Preedit {
                    path: preedit.path.clone(),
                    index: preedit.index,
                    text: preedit.text,
                });
            let active = (caret.at.line == line).then_some(path.as_slice());
            self.draw_line(doc, text, line, here.as_ref(), active)
        };
        let finish = |window: &Range<usize>| {
            self.align_columns(text, window);
            self.rebuild_numbers(window, state.modified);
            if let Some(doc) = self.overlay.owner_document() {
                self.draw_overlay(&doc, state);
                self.draw_ruler(&doc, text.line_count(), state);
            }
        };
        self.viewport
            .show(text, caret.at.line, follow_caret, &draw_line, &finish);
    }

    /// 行番号の幅を、この文書の一番大きい番号に合わせます。番号の幅は文字の測りごとなので、設定は番号を出すかどうかだけを言います。
    fn fit_numbers(&self, count: usize) {
        let style = self.root.style();
        if !crate::settings::line_numbers() {
            if self.gutter_digits.get() != 0 {
                self.gutter_digits.set(0);
                style.remove_property("--setting-gutter").ok();
                self.gutter.set_inner_html("");
            }
            return;
        }
        let digits = count.max(1).to_string().len();
        if self.gutter_digits.get() != digits {
            self.gutter_digits.set(digits);
            let width = format!("calc({digits}ch + 20px)");
            style.set_property("--setting-gutter", &width).ok();
        }
    }

    fn rebuild_numbers(&self, window: &Range<usize>, modified: &[usize]) {
        self.gutter.set_inner_html("");
        if !crate::settings::line_numbers() {
            return;
        }
        let Some(doc) = self.gutter.owner_document() else {
            return;
        };
        let origin = self.gutter.get_bounding_client_rect().top();
        for line in window.clone() {
            let Some(holder) = self.line_element(line) else {
                continue;
            };

            // 変更行の連続した縦ラインを描画（modified はソート済みなので二分探索）
            if modified.binary_search(&line).is_ok() {
                let holder_rect = holder.get_bounding_client_rect();
                let line_top = holder_rect.top() - origin;
                let line_height = holder_rect.height();
                if let Some(change_bar) = element(&doc, "div", "mn-gutter-change") {
                    change_bar
                        .set_attribute(
                            "style",
                            &format!("top:{line_top}px;height:{line_height}px;"),
                        )
                        .ok();
                    append(&self.gutter, &change_bar);
                }
            }

            let Some(rect) = measure::first_base_fragment(&holder) else {
                continue;
            };
            let Some(number) = element(&doc, "span", NUMBER_CLASS) else {
                continue;
            };
            number.set_text_content(Some(&(line + 1).to_string()));
            let top = rect.top + rect.height / 2.0 - origin;
            number.set_attribute("style", &format!("top:{top}px")).ok();
            append(&self.gutter, &number);
        }
    }

    /// スクロールバーのトラック上に変更行・検索ヒット位置のマーカー（Overview Ruler）を描画します。
    fn draw_ruler(&self, doc: &Document, total_lines: usize, state: &Overlay<'_>) {
        let next_highlights: Vec<usize> = state.highlights.iter().map(|hl| hl.line).collect();
        {
            let cache = self.ruler_cache.borrow();
            if cache.total_lines == total_lines
                && cache.modified.as_slice() == state.modified
                && cache.highlights == next_highlights
            {
                // 差分がなければ DOM 再生成をスキップ
                return;
            }
        }

        self.ruler.set_inner_html("");
        if total_lines == 0 {
            *self.ruler_cache.borrow_mut() = RulerCache {
                total_lines: 0,
                modified: Vec::new(),
                highlights: Vec::new(),
            };
            return;
        }
        let total = total_lines as f64;

        // 連続する変更行をグループ化して、途切れずにつながるバーとして描画
        let mut spans: Vec<(usize, usize)> = Vec::new();
        for &line in state.modified {
            if let Some(last) = spans.last_mut() {
                if last.1 + 1 == line {
                    last.1 = line;
                    continue;
                }
            }
            spans.push((line, line));
        }

        for (start, end) in spans {
            let top_pct = (start as f64) / total * 100.0;
            let height_pct = ((end - start + 1) as f64) / total * 100.0;
            if let Some(mark) = element(doc, "div", "mn-ruler-item mn-ruler-modified") {
                mark.set_attribute(
                    "style",
                    &format!("top:{top_pct}%;height:max(3px, {height_pct}%);"),
                )
                .ok();
                append(&self.ruler, &mark);
            }
        }

        // 検索一致マーカー (黄/amber)
        for hl in state.highlights {
            let top_pct = (hl.line as f64 + 0.5) / total * 100.0;
            if let Some(mark) = element(doc, "div", "mn-ruler-item mn-ruler-highlight") {
                mark.set_attribute("style", &format!("top:{top_pct}%;"))
                    .ok();
                append(&self.ruler, &mark);
            }
        }

        *self.ruler_cache.borrow_mut() = RulerCache {
            total_lines,
            modified: state.modified.to_vec(),
            highlights: next_highlights,
        };
    }

    fn draw_line(
        &self,
        doc: &Document,
        text: &Text,
        line: usize,
        preedit: Option<&Preedit<'_>>,
        active: Option<&crate::view::row::Path>,
    ) -> Option<Element> {
        let holder = element(doc, "div", LINE_CLASS)?;
        holder.set_attribute(LINE_ATTR, &line.to_string()).ok();
        let renderer = Renderer::new(doc)
            .with_preedit(preedit)
            .with_active_path(active);
        append(&holder, &renderer.line(text.line(line)));
        Some(holder)
    }

    /// 隣接する行の列区切り文字を揃えます。これは、構造内の行になく行にできることの 1 つです。一度に数行行われますが、これは文書だけが持つことです。
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
            // 一度に 1 列です。列の幅を広げるとその後の列が移動し、測定値もそれに従う必要があるためです。
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

    /// 検索一致、選択、キャレットを、描画済みDOMから測った矩形として重ねる。
    fn draw_overlay(&self, doc: &Document, state: &Overlay<'_>) {
        let sels = state.sels;
        let highlights = state.highlights;
        let caret = state.primary;
        let carets = state.carets;
        let focused = state.focused;
        let linked = state.linked;
        self.overlay.set_inner_html("");
        let origin = self.document.get_bounding_client_rect();
        for highlight in highlights {
            for rect in self.span_in_row(
                highlight.line,
                &highlight.path,
                highlight.from,
                Some(highlight.to),
            ) {
                self.shade_as(doc, rect, &origin, "mn-search-hit");
            }
        }
        // IME の作成中だけキャレットを隠す。非アクティブなペインは、
        // Alt+クリックで同じ入力グループに入ったときだけ表示する。
        let show_carets = (focused || linked) && caret.composing.is_none();
        for (index, nested) in carets.iter().enumerate() {
            let Some(cursor) = nested.inside else {
                continue;
            };
            if !cursor.is_caret() {
                let (path, _) = nested.place();
                for rect in
                    self.span_in_row(nested.at.line, &path, cursor.start(), Some(cursor.end()))
                {
                    self.shade(doc, rect, &origin);
                }
            }
            if show_carets {
                let (path, at) = nested.place();
                if let Some(rect) = self.place_box(nested.at.line, &path, at) {
                    self.mark_caret(doc, rect, &origin, index + 1 == carets.len(), focused);
                }
            }
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
                self.mark_caret(
                    doc,
                    rect,
                    &origin,
                    caret.inside.is_none() && index + 1 == sels.len(),
                    focused,
                );
            }
        }
    }

    fn shade(&self, doc: &Document, rect: Box2, origin: &web_sys::DomRect) {
        self.shade_as(doc, rect, origin, "mn-sel");
    }

    fn shade_as(&self, doc: &Document, rect: Box2, origin: &web_sys::DomRect, class: &str) {
        if let Some(shade) = element(doc, "div", class) {
            set_box(&shade, rect.fix(), origin);
            append(&self.overlay, &shade);
        }
    }

    fn mark_caret(
        &self,
        doc: &Document,
        rect: Box2,
        origin: &web_sys::DomRect,
        primary: bool,
        focused: bool,
    ) {
        let Some(caret) = element(doc, "div", "mn-cursor") else {
            return;
        };
        if !focused {
            caret.class_list().add_1("mn-cursor-inactive").ok();
        } else if primary {
            caret.class_list().add_1("mn-cursor-primary").ok();
        }
        set_box(&caret, Box2 { width: 2.0, ..rect }.fix(), origin);
        append(&self.overlay, &caret);
    }

    fn selection_rects(&self, sel: Sel) -> Vec<Box2> {
        let (start, end) = (sel.start(), sel.end());
        // ページに出ている行しか描けないので、描いた窓と重なる行だけを見る。
        // 全選択で文書全体を回ると、それだけで画面が止まる。
        let drawn = self.viewport.drawn();
        let first = start.line.max(drawn.start);
        let last = end.line.min(drawn.end.saturating_sub(1));
        let mut rects = Vec::new();
        for line in first..=last {
            let from = if line == start.line { start.col } else { 0 };
            let to = (line == end.line).then_some(end.col);
            rects.extend(self.span_in_row(line, &[], from, to));
        }
        rects
    }

    /// 選択範囲が 1 行でカバーする四角形。通常は 1 つですが、その下に運ばれるラインは一度に 1 つずつ、運ばれるラインごとに 1 つずつカバーされます。それぞれの高さは、行の高さではなく、その範囲と同じです。分数を選択すると、分数全体が影になります。単語を選択すると、単語全体が影になります。
    fn span_in_row(&self, line: usize, path: &Path, from: usize, to: Option<usize>) -> Vec<Box2> {
        let Some(row) = self.row_element(line, path) else {
            return Vec::new();
        };
        // 行末を超えて選択すると、改行がギャップとして表示されます。
        let past_end = to.is_none();
        measure::span_boxes(&row, from, to.unwrap_or(usize::MAX), past_end)
    }

    fn caret_rect(&self, at: Pos) -> Option<Box2> {
        self.place_box(at.line, &[], at.col)
    }

    /// 行内の場所が画面上にある場所。 `usize::MAX` はその終わりを意味します。
    fn place_box(&self, line: usize, path: &Path, index: usize) -> Option<Box2> {
        let row = self.row_element(line, path)?;
        measure::boundary(&row, index)
    }

    fn line_element(&self, line: usize) -> Option<Element> {
        self.viewport.line_element(line)
    }

    fn line_row(&self, line: usize) -> Option<Element> {
        self.row_element(line, &[])
    }

    fn row_element(&self, line: usize, path: &Path) -> Option<Element> {
        let holder = self.line_element(line)?;
        let selector = format!("[{PATH_ATTR}=\"{}\"]", row::encode_path(path));
        holder.query_selector(&selector).ok().flatten()
    }

    /// クリックが到達したドキュメント内の場所（深さは問いません）。ポイントが存在する最も内側の行が決定するため、分母をクリックすると、分数の横ではなく分母に到達します。
    pub fn hit(&self, text: &Text, x: f64, y: f64) -> Hit {
        // ページ内にあるもののみがヒットします。したがって、クリックは描画された線で応答されます。とにかく、他のすべてはポインタの下にありません。
        let drawn = self.viewport.drawn();
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

    /// テキスト内のクリックが着地した場所。島全体を示します。
    pub fn pos_at_point(&self, text: &Text, x: f64, y: f64) -> Pos {
        match self.hit(text, x, y) {
            Hit::Text(at) => at,
            // 島内の点は島にあり、ポインタが右半分に来ると島を通過するため、1 つ上をドラッグすると島が取り込まれます。
            Hit::Inside(at, _) => match self.node_box(at) {
                Some(rect) if x > rect.left + rect.width / 2.0 => Pos::new(at.line, at.col + 1),
                _ => at,
            },
        }
    }

    fn node_box(&self, at: Pos) -> Option<Box2> {
        let row = self.line_row(at.line)?;
        let children = row.children();
        for i in 0..children.length() {
            let child = children.item(i)?;
            if measure::start_of(&child) == Some(at.col) {
                return Some(measure::box_of(&child.get_bounding_client_rect()));
            }
        }
        None
    }

    /// キャレットが描画される場所は、テキスト内の場所であっても、そこに立っている構造内の場所であっても構いません。
    fn caret_box(&self, caret: &Caret<'_>) -> Option<Box2> {
        let (path, index) = caret.place();
        self.place_box(caret.at.line, &path, index)
    }

    /// スクロールしてキャレットが見えるようにし、入力要素がキャレットに従うことができるように**文書内の**場所を報告します (ここに IME 候補が表示されます)。画面ではなくドキュメント: input 要素は行の間に配置され、行と一緒にスクロールします。ドキュメントの上部に残された input 要素は、入力されるとすぐにブラウザがスクロールして戻ってくるものです。
    pub fn reveal(&self, caret: &Caret<'_>) -> Option<Box2> {
        let rect = self.caret_box(caret)?;
        Some(self.viewport.reveal(&self.scroller, rect))
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

pub(super) fn has_tab(nodes: &[Node]) -> bool {
    nodes.iter().any(|node| matches!(node.kind, NodeKind::Tab))
}

fn children_of_class(holder: &Element, class: &str) -> Vec<Element> {
    let children = holder.children();
    (0..children.length())
        .filter_map(|i| children.item(i))
        .filter(|child| child.class_list().contains(class))
        .collect()
}

pub(super) fn append(parent: &impl AsRef<web_sys::Node>, child: &Element) {
    parent.as_ref().append_child(child.as_ref()).ok();
}

pub(super) fn element(doc: &Document, tag: &str, class: &str) -> Option<Element> {
    let element = doc.create_element(tag).ok()?;
    element.set_class_name(class);
    Some(element)
}
