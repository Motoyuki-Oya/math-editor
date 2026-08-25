//! ファイルを覗く窓。文書全体をブラウザーに置かず、見えている行だけを描く。
//!
//! 縦のスクロールはブラウザーに任せない。窓の先頭の行 `top` がすべてを決め、
//! ホイールは行数で、つまみは文書全体の割合で `top` を動かす。つまみのための
//! 細い要素だけが数千 px の空間を持ち、行数 × 行高の巨大な要素はどこにも
//! 作らない。キャレットを追う描画はキャレットの行が見えて終わり、追わない
//! 描画は `top` の窓を描き直すだけで、スクロール座標から窓を逆算しない。

use std::cell::{Cell, RefCell};
use std::ops::Range;

use web_sys::{Document, Element, HtmlElement};

use crate::structure::text::Text;
use crate::view::heights::Heights;
use crate::view::measure::{self, Box2};

use super::document::{append, has_tab, LINE_ATTR};

/// 画面を満たす行数に足す余分。端で半分だけ見える行と、少しの先読み。
const OVERSCAN: usize = 4;
/// 列区切りのブロックが描画範囲を画面の外へ引き込める行数。列を揃えるには
/// ブロック全体が要りますが、いくらでもというわけにはいきません。
const BLOCK_LIMIT: usize = 200;
/// つまみのための空間。ウィンドウの 6 倍か、最低でもこの高さ。
const THUMB_SCREENS: f64 = 6.0;
const THUMB_MIN: f64 = 4_000.0;

pub(super) struct Viewport {
    /// エディター全体の箱。大きさを測るためだけに見る。
    root: HtmlElement,
    /// 描いた行が入る要素。窓の行だけが入り、場所取りは無い。
    document: Element,
    /// つまみだけのための細いスクロール領域。
    scrollbar: HtmlElement,
    /// つまみの領域の中身。高さだけを持つ。
    thumb_space: Element,
    /// 各行の高さ。窓に何行入るかの見積もりに使います。
    heights: RefCell<Heights>,
    /// 現在ページ内にある行。ページを測定するものはすべて、これらについてのみ
    /// 語ることができます。
    drawn: RefCell<Range<usize>>,
    /// 窓の先頭の行。
    top: Cell<usize>,
    /// 文書の行数。`top` を丸めるために、描くたびに控える。
    count: Cell<usize>,
    /// こちらから合わせたつまみの位置。つまみ自身のイベントの反響を無視する。
    thumb_echo: Cell<f64>,
}

impl Viewport {
    pub(super) fn new(
        root: HtmlElement,
        document: Element,
        scrollbar: HtmlElement,
        thumb_space: Element,
    ) -> Self {
        Self {
            root,
            document,
            scrollbar,
            thumb_space,
            heights: RefCell::new(Heights::new()),
            drawn: RefCell::new(0..0),
            top: Cell::new(0),
            count: Cell::new(1),
            thumb_echo: Cell::new(0.0),
        }
    }

    pub(super) fn drawn(&self) -> Range<usize> {
        self.drawn.borrow().clone()
    }

    /// 描いてある窓を忘れさせ、次の描き直しに必ず描かせる。窓の中の行の
    /// 中身が変わった（届いた）ときに使う。
    pub(super) fn invalidate(&self) {
        *self.drawn.borrow_mut() = 0..0;
    }

    /// 行の要素は、子の中での位置ではなく、その行が表す行によって決まります。
    /// ページ内には行の一部のみが存在します。
    pub(super) fn line_element(&self, line: usize) -> Option<Element> {
        self.document
            .query_selector(&format!("[{LINE_ATTR}=\"{line}\"]"))
            .ok()
            .flatten()
    }

    /// ホイールの分だけ窓を動かす。描き直しは呼び出し側が行う。
    pub(super) fn nudge(&self, pixels: f64) {
        let unit = self.unit();
        let lines = (pixels.abs() / unit).round().max(1.0) as usize;
        let top = self.top.get();
        self.top.set(if pixels < 0.0 {
            top.saturating_sub(lines)
        } else {
            (top + lines).min(self.count.get().saturating_sub(1))
        });
    }

    /// つまみの位置。こちらから合わせた反響なら `None`。
    pub(super) fn thumb_ratio(&self) -> Option<f64> {
        let at = self.scrollbar.scroll_top() as f64;
        if (at - self.thumb_echo.get()).abs() <= 2.0 {
            return None;
        }
        let max = (self.scrollbar.scroll_height() - self.scrollbar.client_height()) as f64;
        Some((at / max.max(1.0)).clamp(0.0, 1.0))
    }

    /// つまみの割合の場所へ窓を持っていく。描き直しは呼び出し側が行う。
    pub(super) fn jump_to_ratio(&self, ratio: f64) {
        let count = self.count.get();
        self.top
            .set((ratio * count.saturating_sub(1) as f64).round() as usize);
    }

    /// ページを `text` に合わせて描き直します。`follow` の描画はキャレットの行
    /// （`caret_line`）が見えて終わり、そうでない描画は窓をその場で描き直します。
    /// 行を描くのは `draw_line`、描き終えた窓に対する仕上げ（列揃えとキャレット）
    /// は `finish` が行います。
    pub(super) fn show(
        &self,
        text: &Text,
        caret_line: usize,
        follow: bool,
        draw_line: &dyn Fn(&Document, usize) -> Option<Element>,
        finish: &dyn Fn(&Range<usize>),
    ) {
        let count = text.line_count();
        self.count.set(count);
        self.heights.borrow_mut().fit(count);
        self.top.set(self.top.get().min(count.saturating_sub(1)));
        if follow && !self.drawn.borrow().contains(&caret_line) {
            // 遠くへの跳び。キャレットの行を上から 1/3 に置く。
            let fit = self.lines_that_fit();
            self.top.set(caret_line.saturating_sub(fit / 3));
        }
        let window = self.widen_for_blocks(text, self.window(count));
        let drawn = self.drawn.borrow().clone();
        if !follow && drawn == window {
            return;
        }
        self.place(window, draw_line, finish);
        if follow {
            self.keep_caret_visible(text, caret_line, draw_line, finish);
        }
        self.sync_thumb(count);
    }

    /// 窓: `top` から画面を満たすだけの行。
    fn window(&self, count: usize) -> Range<usize> {
        let top = self.top.get().min(count.saturating_sub(1));
        let need = self.lines_that_fit() + OVERSCAN;
        top..(top + need).min(count)
    }

    /// 画面に入る行数の見積もり。
    fn lines_that_fit(&self) -> usize {
        let view = self.root.client_height() as f64;
        ((view / self.unit()).ceil() as usize).max(1)
    }

    fn unit(&self) -> f64 {
        let heights = self.heights.borrow();
        let drawn = self.drawn.borrow().clone();
        let sample = heights.span(drawn.clone());
        if drawn.is_empty() || sample <= 0.0 {
            20.0
        } else {
            (sample / drawn.len() as f64).max(1.0)
        }
    }

    /// 窓の行を描いて測る。
    fn place(
        &self,
        window: Range<usize>,
        draw_line: &dyn Fn(&Document, usize) -> Option<Element>,
        finish: &dyn Fn(&Range<usize>),
    ) {
        let Some(doc) = self.document.owner_document() else {
            return;
        };
        self.document.set_inner_html("");
        for line in window.clone() {
            if let Some(element) = draw_line(&doc, line) {
                append(&self.document, &element);
            }
        }
        *self.drawn.borrow_mut() = window.clone();
        self.measure(&window);
        finish(&window);
    }

    /// キャレットの行が画面の中に入るまで、窓を少しずつずらして描き直す。
    /// 行の高さは描くまで分からないので、一度では入らないことがある。
    fn keep_caret_visible(
        &self,
        text: &Text,
        caret_line: usize,
        draw_line: &dyn Fn(&Document, usize) -> Option<Element>,
        finish: &dyn Fn(&Range<usize>),
    ) {
        let count = text.line_count();
        for _ in 0..4 {
            let Some(holder) = self.line_element(caret_line) else {
                // 窓の見積もりが小さすぎてキャレットの行が入らなかった。
                // キャレットを窓の先頭へ置けば必ず入る。
                if self.top.get() == caret_line {
                    return;
                }
                self.top.set(caret_line);
                let window = self.widen_for_blocks(text, self.window(count));
                self.place(window, draw_line, finish);
                continue;
            };
            let rect = measure::box_of(&holder.get_bounding_client_rect());
            let view = measure::box_of(&self.root.get_bounding_client_rect());
            let unit = self.unit();
            if rect.top < view.top - 0.5 {
                // 上へはみ出した。キャレットの行を先頭にする。
                self.top.set(caret_line);
            } else if rect.top + rect.height > view.top + self.root.client_height() as f64 {
                // 下へはみ出した分だけ窓を下げる。
                let overflow = rect.top + rect.height - view.top - self.root.client_height() as f64;
                let lines = ((overflow / unit).ceil() as usize).max(1);
                self.top
                    .set((self.top.get() + lines).min(count.saturating_sub(1)));
            } else {
                return;
            }
            let window = self.widen_for_blocks(text, self.window(count));
            self.place(window, draw_line, finish);
        }
    }

    /// つまみを窓の位置に合わせる。つまみの空間の高さもここで整える。
    fn sync_thumb(&self, count: usize) {
        let space = (self.root.client_height() as f64 * THUMB_SCREENS).max(THUMB_MIN);
        self.thumb_space
            .set_attribute("style", &format!("height:{space}px;width:1px"))
            .ok();
        let max = (self.scrollbar.scroll_height() - self.scrollbar.client_height()) as f64;
        let ratio = if count <= 1 {
            0.0
        } else {
            self.top.get() as f64 / (count - 1) as f64
        };
        let target = (ratio * max.max(0.0)).round();
        if (self.scrollbar.scroll_top() as f64 - target).abs() > 0.5 {
            self.thumb_echo.set(target);
            self.scrollbar.set_scroll_top(target as i32);
        }
    }

    /// 描画範囲を列区切りのブロック全体に広げます。列を揃えるにはブロックの
    /// すべての行が要るからです。
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

    /// 描いた行の高さを記録します。
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

    /// キャレットの箱（画面座標）を横に見える範囲へ入れ、その箱の**文書内の**
    /// 場所を返します。縦はキャレットを追う描き直しが窓ごと合わせるので、
    /// ここでは動かしません。
    pub(super) fn reveal(&self, scroller: &HtmlElement, rect: Box2) -> Box2 {
        let view = measure::box_of(&scroller.get_bounding_client_rect());
        let scroll_left = scroller.scroll_left() as f64;
        let top = rect.top - view.top;
        let left = rect.left - view.left + scroll_left;
        let width = scroller.client_width() as f64;
        if left < scroll_left {
            scroller.set_scroll_left((left - 24.0).max(0.0) as i32);
        } else if left > scroll_left + width - 24.0 {
            scroller.set_scroll_left((left - width + 24.0) as i32);
        }
        Box2 { left, top, ..rect }
    }
}
