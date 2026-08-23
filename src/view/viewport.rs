//! ページに出す行の窓と、ビューをどこへ持っていくか。
//!
//! 見えている行だけを描くための帳簿がすべてここにある。どの行を描くか（窓）、
//! 描かれていない行の場所取り（上下の空の要素）、測った高さの記録
//! （[`super::heights`]）、そしてスクロールの行き先。行そのものをどう描くかは
//! [`super::document`] の仕事で、ここは描き手に「この範囲を描いて」と頼むだけ。
//!
//! ビューの行き先の規則は 1 つ: **キャレットを追う描画はキャレットの行が見えて
//! 終わり、追わない描画は見えていた行を見失わない。** 高さは描くまで推測なので、
//! 一度で正しい位置には着けないことがある。そのときは描いて測った高さで行き先を
//! 決め直し、動かなくなるまで繰り返す。

use std::cell::RefCell;
use std::ops::Range;

use web_sys::{Document, Element, HtmlElement};

use crate::structure::text::Text;
use crate::view::heights::Heights;
use crate::view::measure::{self, Box2};

use super::document::{append, element, has_tab, LINE_ATTR};

/// ページ内にない行を表し、ドキュメントの全長が維持されます。
const GAP_CLASS: &str = "mn-gap";

/// 画面の端を越えてどのくらい先まで描くか（画面数）。これより少ないスクロールは、
/// すでにそこにある行が受け止めます。
const MARGIN_SCREENS: f64 = 1.0;
/// ページの高さの上限。ブラウザーは要素の高さに上限（数千万 px）があり、
/// 何百万行の文書を 1 行 1 行の高さで積むと超えてしまう。超えるときは
/// 置き場所を同じ比率で縮め、スクロール位置と行の対応も同じ比率で読む。
const MAX_PIXELS: f64 = 16_000_000.0;
/// 列区切りのブロックが描画範囲を画面の外へ引き込める行数。列を揃えるには
/// ブロック全体が要りますが、いくらでもというわけにはいきません。
const BLOCK_LIMIT: usize = 200;

pub(super) struct Viewport {
    /// スクロールする要素。
    root: HtmlElement,
    /// 描いた行が入る要素。上下の場所取りもここに入ります。
    document: Element,
    /// 各行の高さ。それによって描画する行が決まります。
    heights: RefCell<Heights>,
    /// 現在ページ内にある行。ページを測定するものはすべて、これらについてのみ
    /// 語ることができます。
    drawn: RefCell<Range<usize>>,
    /// 置き場所の縮尺。文書の画素の高さがブラウザーの上限に収まるときは 1。
    scale: std::cell::Cell<f64>,
}

impl Viewport {
    pub(super) fn new(root: HtmlElement, document: Element) -> Self {
        Self {
            root,
            document,
            heights: RefCell::new(Heights::new()),
            drawn: RefCell::new(0..0),
            scale: std::cell::Cell::new(1.0),
        }
    }

    pub(super) fn drawn(&self) -> Range<usize> {
        self.drawn.borrow().clone()
    }

    /// 行の要素は、子の中での位置ではなく、その行が表す行によって決まります。
    /// ページ内には行の一部のみが存在します。
    pub(super) fn line_element(&self, line: usize) -> Option<Element> {
        self.document
            .query_selector(&format!("[{LINE_ATTR}=\"{line}\"]"))
            .ok()
            .flatten()
    }

    /// ページを `text` に合わせて描き直します。`follow` の描画はキャレットの行
    /// （`caret_line`）が見えて終わり、そうでない描画はユーザーの置いたスクロールを
    /// 尊重します。行を描くのは `draw_line`、描き終えた窓に対する仕上げ（列揃えと
    /// キャレット）は `finish` が行います。
    pub(super) fn show(
        &self,
        text: &Text,
        caret_line: usize,
        follow: bool,
        draw_line: &dyn Fn(&Document, usize) -> Option<Element>,
        finish: &dyn Fn(&Range<usize>),
    ) {
        {
            let mut heights = self.heights.borrow_mut();
            heights.fit(text.line_count());
            // 文書の画素の高さがブラウザーの上限を超えるなら、置き場所を縮める。
            let total = heights.span(0..text.line_count());
            self.scale.set((MAX_PIXELS / total.max(1.0)).min(1.0));
        }
        // ビューの行き先: 何かが変わったときはキャレット、スクロールされたときは
        // ユーザーが置いた場所。
        let mut scroll = match follow {
            true => self.scroll_for(caret_line, text.line_count()),
            false => self.root.scroll_top() as f64,
        };
        // キャレットの行がいま見えているかどうか。追う描画は見えて終わらなければ
        // ならず、追わない描画も少なくとも見失ってはなりません。文書の末尾では
        // 高さを測り直すとスクロールが行の分だけ動けないことがあるからです。
        let mut keep = follow;
        if !follow {
            let window = self.widen_for_blocks(text, self.window(scroll, text.line_count()));
            // 余白の中のスクロールは、すでにそこにある行が受け止めるので、
            // 描くものがありません。
            if window == *self.drawn.borrow() {
                return;
            }
            keep = (self.scroll_onto(caret_line, scroll) - scroll).abs() <= 0.5;
        }
        scroll = self.place(text, scroll, draw_line, finish);
        if !keep {
            return;
        }
        // 上のスクロールはまだ推測の高さから計算されているので、キャレットの行は
        // 推測と違う場所に着くことがあります。描いて測った行から行き先を決め直し、
        // 動かなくなるまで繰り返します。推測のままの位置をユーザーに見せたままには
        // しません。
        for _ in 0..3 {
            let settled = self.scroll_onto(caret_line, scroll);
            if (settled - scroll).abs() <= 0.5 {
                return;
            }
            scroll = self.place(text, settled, draw_line, finish);
        }
    }

    /// `scroll` の窓の行をページに置き、ビューをそこへ残し、実際にどこへ着いたかを
    /// 返します。
    fn place(
        &self,
        text: &Text,
        scroll: f64,
        draw_line: &dyn Fn(&Document, usize) -> Option<Element>,
        finish: &dyn Fn(&Range<usize>),
    ) -> f64 {
        let Some(doc) = self.document.owner_document() else {
            return self.root.scroll_top() as f64;
        };
        let scale = self.scale.get();
        let window = self.widen_for_blocks(text, self.window(scroll, text.line_count()));
        self.document.set_inner_html("");
        let above = element(&doc, "div", GAP_CLASS);
        // 描く行の上に何があるかの見立て。測ると変わり、描いた行もその差の分だけ
        // 動くので、スクロールも一緒に動かします。場所取りは縮尺つき。
        let guessed = self.heights.borrow().span(0..window.start) * scale;
        if let Some(gap) = &above {
            // 場所取りはページに入る前に高さを持ちます。文書より短いページは
            // ブラウザがスクロールを切り詰め、ビューがそれ以上下へ行けなく
            // なるからです。
            set_height(gap, guessed);
            append(&self.document, gap);
        }
        for line in window.clone() {
            if let Some(element) = draw_line(&doc, line) {
                append(&self.document, &element);
            }
        }
        let below = element(&doc, "div", GAP_CLASS);
        if let Some(gap) = &below {
            set_height(
                gap,
                self.heights.borrow().span(window.end..text.line_count()) * scale,
            );
            append(&self.document, gap);
        }
        *self.drawn.borrow_mut() = window.clone();
        self.measure(&window);
        // 間の行を測り直したので、場所取りも合わせ直します。
        let heights = self.heights.borrow();
        let measured = heights.span(0..window.start) * scale;
        if let Some(gap) = &above {
            set_height(gap, measured);
        }
        if let Some(gap) = &below {
            set_height(gap, heights.span(window.end..text.line_count()) * scale);
        }
        drop(heights);
        // ビューの行き先はここでだけ決まります。行を置き換えるとページが一時的に
        // 短くなってスクロールが切り詰められることがあり、切り詰められたスクロールは
        // たまたま描いた行の先へは行けません。場所取りが測定で伸び縮みすると行も
        // 動くので、同じ量だけスクロールを動かして画面の内容をその場に留めます。
        let scroll = (scroll + measured - guessed).max(0.0);
        if (self.root.scroll_top() as f64 - scroll).abs() > 0.5 {
            self.root.set_scroll_top(scroll as i32);
        }
        finish(&window);
        // 要求した値ではなく、ブラウザが受け入れた値。文書の末尾はそこまでです。
        self.root.scroll_top() as f64
    }

    /// 行を丸ごと見せるスクロール。行がすでに見えていれば `scroll` のまま。描いた
    /// 行を測るので、その上の行の推測が答えを動かすことはありません。視界に入れる
    /// 行は一行分だけ余計に入れます。ビューの端とぴったりの行は無いのと同じに
    /// 読めるからで、文書の末尾では余りをブラウザが切り詰めます。
    fn scroll_onto(&self, line: usize, scroll: f64) -> f64 {
        let view = self.root.client_height() as f64;
        let Some(holder) = self.line_element(line) else {
            // ビューを動かす先の行がまだ描かれてもいないので、範囲を選んだ高さが
            // その行について間違っていました。それまでに測ったものからもう一度
            // 狙い直します。
            return self.guessed_scroll(line, view);
        };
        let rect = measure::box_of(&holder.get_bounding_client_rect());
        let root = measure::box_of(&self.root.get_bounding_client_rect());
        // 画面上のものを、スクロール自身の尺度で。
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

    /// 行の描かれていないときの行き先。置き場所の縮尺で読む。
    fn guessed_scroll(&self, line: usize, view: f64) -> f64 {
        (self.heights.borrow().top_of(line) * self.scale.get() - view / 3.0).max(0.0)
    }

    /// 画面が届く行に加えて上下に一画面分。少しのスクロールでは何も描き直さずに
    /// 済むようにです。
    fn window(&self, scroll: f64, count: usize) -> Range<usize> {
        let height = self.root.client_height() as f64;
        let margin = (height * MARGIN_SCREENS).max(200.0);
        let heights = self.heights.borrow();
        // スクロール位置は縮尺つきの置き場所で行に読み替え、そこから
        // 上下の余白と画面のぶんは実寸の高さで数える。
        let anchor = heights.line_at(scroll / self.scale.get());
        let top = heights.top_of(anchor);
        let start = heights.line_at(top - margin);
        let end = heights.line_at(top + height + margin) + 1;
        start..end.max(start + 1).min(count)
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

    /// 行を描くにはビューがどこに無ければならないか。次の窓がすでに届く行なら
    /// ビューはそのまま。一行ずつキャレットへ寄るのは [`Self::reveal`] の仕事で、
    /// これはそれより遠い行（Ctrl+End、検索の一致、長い貼り付け）のためのものです。
    /// 描かれていない行は測れないので、先にビューを動かす必要があります。
    fn scroll_for(&self, line: usize, count: usize) -> f64 {
        let scroll = self.root.scroll_top() as f64;
        if self.window(scroll, count).contains(&line) {
            return scroll;
        }
        // 三分の一ほど下に置いて、キャレットに続くものが見えるようにします。
        self.guessed_scroll(line, self.root.client_height() as f64)
    }

    /// キャレットの箱（画面座標）が見える範囲に入るまでスクロールし、その箱の
    /// **文書内の**場所を返します。
    pub(super) fn reveal(&self, rect: Box2) -> Box2 {
        let view = measure::box_of(&self.root.get_bounding_client_rect());
        let scroll = (
            self.root.scroll_top() as f64,
            self.root.scroll_left() as f64,
        );
        let top = rect.top - view.top + scroll.0;
        let left = rect.left - view.left + scroll.1;
        // 見えるのはクライアントの箱。スクロールバーが占める分はキャレットを
        // 見せられる場所ではありません。
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
        Box2 { left, top, ..rect }
    }
}

fn set_height(element: &Element, height: f64) {
    element
        .set_attribute("style", &format!("height:{height}px"))
        .ok();
}
