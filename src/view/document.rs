//! ドキュメント、キャレット、選択範囲をページに描画します。
//!
//! すべての行は、行を描画する 1 つのコンポーネントである [`crate::view::row`] によって描画されるため、テキストの行と構造の内部は、ここでのすべてのものと同じです。構造内の行にできない唯一のことは、列セパレータを隣接する行と並べることです。
//!
//! 描画されたものがどこに到達したかは、[`crate::view::measure`] によって読み戻されます。画面上に何かを配置することと、そこにあるものを測定することは反対の仕事であるため、それらは離れたままになります。
//!
//! ここにあるものはブラウザでは編集できません。行は単純なスパンであり、すべてのキャレットは絶対に配置された小さな要素です。キャレットは一度に表示でき、構造内のキャレットがテキスト内のキャレットとどのように同じであるかについて説明します。
//!
//! ページ内には表示される行のみが表示されます。それらの上下には、それらが表す行と同じ高さの 2 つの空の要素が配置されているため、ページに 1 画面分の文書が含まれている間、文書は全長にスクロールします。線の高さは一度測定されると維持されます。決して描かれていない線は推測されるため、文書がスクロールされるとスクロールバーが固定されます。

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
/// 線の横に表示される数字。これは行の外側のマージンに配置されるため、テキストを測定する人が行の一部と間違えることはありません。
const NUMBER_CLASS: &str = "mn-number";
/// ページ内にない行を表し、ドキュメントの全長が維持されます。
const GAP_CLASS: &str = "mn-gap";

/// スクリーン線を超えてどのくらいの距離が描画されるか (画面単位)。これより少ない量でスクロールすると、すでにそこにある行が表示されます。
const MARGIN_SCREENS: f64 = 1.0;
/// 列セパレータのブロックが描画範囲を画面を超えて引き込むことができる距離。列を並べるにはブロック全体が必要ですが、何の代償もかかりません。
const BLOCK_LIMIT: usize = 200;

pub struct View {
    /// スクロールし、マウスを受け取ります。
    pub root: HtmlElement,
    lines: Element,
    overlay: Element,
    /// 各行の高さ。それによって描画する線が決まります。
    heights: RefCell<Heights>,
    /// 現在ページ内にある行。ページを測定するものはすべて、これらについてのみ語ることができます。
    drawn: RefCell<Range<usize>>,
}

/// キャレットがどこにあるか、描画がそれについて知る必要があるのはそれだけです。テキスト内の場所、キャレットがそこにある構造のどの深さまで到達しているか、IME がそこで何を構成しているかです。モードはありません。同じキャレットで両方のケースを説明します。
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
            Some(cursor) => {
                let mut path = vec![(self.at.col, 0)];
                path.extend_from_slice(&cursor.path);
                (path, cursor.index)
            }
        }
    }
}

impl View {
    /// 呼び出し元が所有する `root` 内にレイヤーを構築します。
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

    /// 変更後に描画し、キャレットの行をページ内に移動します。
    pub fn draw(&self, text: &Text, sels: &[Sel], caret: &Caret<'_>, focused: bool) {
        self.paint(text, sels, caret, focused, true);
    }

    /// スクロール後に描画します。これによりビューがキャレットに移動してはなりません。スクロールバーはキャレットではなくユーザーのものです。
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
        // ビューはどこにあるのか内容は次のようになります: 何かが変更されたときのキャレット、およびユーザーがスクロールしたときにそこから離れた場所。
        let mut scroll = match follow_caret {
            true => self.scroll_for(caret.at.line, text.line_count()),
            false => self.root.scroll_top() as f64,
        };
        // キャレットの行が現在表示されるかどうか。キャレットに続く描画は、キャレットが見えた状態で終了する必要があります。描画は、少なくとも見失ってはなりません。これは、文書の最後で行を新たに測定することができます。スクロールは、スペースを確保する必要がある行まで移動できません。
        let mut keep = follow_caret;
        if !follow_caret {
            let window = self.widen_for_blocks(text, self.window(scroll, text.line_count()));
            // 余白内でスクロールすると、すでにそこにある行が表示されるため、描画するものは何もありません。
            if window == *self.drawn.borrow() {
                return;
            }
            keep = (self.scroll_onto(caret.at.line, scroll) - scroll).abs() <= 0.5;
        }
        scroll = self.render(text, sels, caret, focused, scroll);
        if !keep {
            return;
        }
        // 上のスクロールは、描画しようとしている行のまだ推測された高さに基づいて計算されているため、キャレットの行が推測とは異なる場所に到達する可能性があります。したがって、ビューが終了する場所は、移動が停止するまで、描かれた線に基づいて決定されます。ユーザーが見ているままにされることは決してありません。
        for _ in 0..3 {
            let settled = self.scroll_onto(caret.at.line, scroll);
            if (settled - scroll).abs() <= 0.5 {
                return;
            }
            scroll = self.render(text, sels, caret, focused, settled);
        }
    }

    /// 線全体を表示するスクロール、または線がすでに見えている場合は `scroll`。描かれた線が測定されるものであるため、その上の線を推測しても答えは変わりません。視界に入った線は、必要以上に遠くに表示されます。これは、ビューの端と同じ高さの線がそこに存在しないものとして読み取られるためです。ドキュメントの最後で、ブラウザは余分な部分をトリミングしますが、パディングによってスペースが確保されます。
    fn scroll_onto(&self, line: usize, scroll: f64) -> f64 {
        let view = self.root.client_height() as f64;
        let Some(holder) = self.line_element(line) else {
            // ビューが移動される線も描かれていないため、範囲を選択した高さが間違っていました。それ以降に測定されたものからもう一度目標を立てます。
            return (self.heights.borrow().top_of(line) - view / 3.0).max(0.0);
        };
        let rect = measure::box_of(&holder.get_bounding_client_rect());
        let root = measure::box_of(&self.root.get_bounding_client_rect());
        // 画面上にあるもの。スクロール自体の尺度で示されます。
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

    /// ページ内に `scroll` の行を配置し、ビューをそこに残し、実際に最終的にどこに到達したかを返します。
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
        // IME が作成しているテキストは、どの行にあってもキャレットに属します。行を描画するコンポーネントがそれをそこに配置します。
        let (path, index) = caret.place();
        let preedit = caret.composing.map(|text| Preedit { path, index, text });
        self.lines.set_inner_html("");
        let above = element(&doc, "div", GAP_CLASS);
        // 描画された行の上にある行が何であるかがわかります。価値がある。測定すると変化し、描画された線もその差分だけ移動するため、スクロールも一緒に移動する必要があります。
        let guessed = self.heights.borrow().span(0..window.start);
        if let Some(gap) = &above {
            // ギャップには、ページ内に配置される前に高さが設定されます。ドキュメントより短いページはブラウザによってスクロールがトリミングされ、ビューがそれ以上下に移動できなくなります。
            set_height(gap, guessed);
            append(&self.lines, gap);
        }
        for line in window.clone() {
            // キャレットがある行のみ、構成内容が表示されます。
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
        // ギャップは、ギャップ間の線が調整されたので再び表示されます。
        let heights = self.heights.borrow();
        let measured = heights.span(0..window.start);
        if let Some(gap) = &above {
            set_height(gap, measured);
        }
        if let Some(gap) = &below {
            set_height(gap, heights.span(window.end..text.line_count()));
        }
        drop(heights);
        // ビューがどこに到達するかは、他の場所ではなくここで決まります。行を置き換えると、スクロールがトリミングされてページが一時的に短くなったままになる可能性があり、トリミングされて戻ったスクロールは、たまたま描画された行を超えてスクロールすることはできません。行の測定に伴って拡大または縮小するギャップによって行も移動し、同じ量だけスクロールを移動すると、画面上の内容が元の位置に保たれます。
        let scroll = (scroll + measured - guessed).max(0.0);
        if (self.root.scroll_top() as f64 - scroll).abs() > 0.5 {
            self.root.set_scroll_top(scroll as i32);
        }
        self.align_columns(text, &window);
        self.draw_carets(&doc, sels, caret, focused);
        // 要求されたものではなく、ブラウザが提供したものです。文書の最後はそこまでです。
        self.root.scroll_top() as f64
    }

    /// 画面が到達する行に加え、上下に画面があるため、少しスクロールするだけでまったく描画する必要がありません。
    fn window(&self, scroll: f64, count: usize) -> Range<usize> {
        let height = self.root.client_height() as f64;
        let margin = (height * MARGIN_SCREENS).max(200.0);
        let heights = self.heights.borrow();
        let start = heights.line_at(scroll - margin);
        let end = heights.line_at(scroll + height + margin) + 1;
        start..end.max(start + 1).min(count)
    }

    /// 描画範囲を列区切りのブロック全体に広げます。列を整列させるのは
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

    /// 描画した線の高さがどれくらいであるかに注目してください。
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

    /// 線を描画するにはどこにビューがなければなりません。次の範囲がすでに到達している行はビューをそのまま残します。一度に 1 行ずつキャレットに移動するのは [`Self::reveal`] の仕事です。それよりも離れた行 (Ctrl+End、検索ヒット、長いペースト) がこれの目的です。描画されていない線は測定できないため、ビューを最初に移動する必要があります。
    fn scroll_for(&self, line: usize, count: usize) -> f64 {
        let scroll = self.root.scroll_top() as f64;
        if self.window(scroll, count).contains(&line) {
            return scroll;
        }
        let height = self.root.client_height() as f64;
        // 3 分の 1 ほど下に移動して、キャレットの後に続くものが表示されるようにします。
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

    /// テキスト内と構造内に同じようにすべてのキャレットとすべての選択範囲を描画します。それらはすべて、描画されたものから測定された長方形です。
    fn draw_carets(&self, doc: &Document, sels: &[Sel], caret: &Caret<'_>, focused: bool) {
        self.overlay.set_inner_html("");
        let origin = self.lines.get_bounding_client_rect();
        // IME の作成中、下線付きのテキストは、
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

    /// 行の要素は、子の中での位置ではなく、その行が表す行によって決まります。ページ内には行の一部のみが存在します。
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

    /// クリックが到達したドキュメント内の場所（深さは問いません）。ポイントが存在する最も内側の行が決定するため、分母をクリックすると、分数の横ではなく分母に到達します。
    pub fn hit(&self, text: &Text, x: f64, y: f64) -> Hit {
        // ページ内にあるもののみがヒットします。したがって、クリックは描画された線で応答されます。とにかく、他のすべてはポインタの下にありません。
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

    /// テキスト内のクリックが着地した場所。島全体を示します。
    pub fn pos_at_point(&self, text: &Text, x: f64, y: f64) -> Pos {
        match self.hit(text, x, y) {
            Hit::Text(at) => at,
            // 島内の点は島にあり、ポインタが右半分に来ると島を通過するため、1 つ上をドラッグすると島が取り込まれます。
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

    /// キャレットが描画される場所は、テキスト内の場所であっても、そこに立っている構造内の場所であっても構いません。
    fn caret_box(&self, caret: &Caret<'_>) -> Option<Box2> {
        let (path, index) = caret.place();
        self.place_box(caret.at.line, &path, index)
    }

    /// スクロールしてキャレットが見えるようにし、入力要素がキャレットに従うことができるように**文書内の**場所を報告します (ここに IME 候補が表示されます)。画面ではなくドキュメント: input 要素は行の間に配置され、行と一緒にスクロールします。ドキュメントの上部に残された input 要素は、入力されるとすぐにブラウザがスクロールして戻ってくるものです。
    pub fn reveal(&self, caret: &Caret<'_>) -> Option<Box2> {
        let rect = self.caret_box(caret)?;
        let view = measure::box_of(&self.root.get_bounding_client_rect());
        let scroll = (
            self.root.scroll_top() as f64,
            self.root.scroll_left() as f64,
        );
        let top = rect.top - view.top + scroll.0;
        let left = rect.left - view.left + scroll.1;
        // 見えるのはクライアント ボックスです。スクロールバーが占めるスペースは、キャレットが表示できるスペースではありません。
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
