//! どのラインが背が高いか、そのラインが座っているか。
//!
//! 目に見える線だけが描かれているため、どの線であるかはページからではなく高さから判断する必要があります。描かれた線には測定された高さがあります。決して引かれなかった線は、測定された線の平均値と見なされます。
//!
//! 「a..b行の高さはどのくらいですか」と「y行はどの行ですか」という両方の質問は、何百万行もある文書をスクロールするたびに尋ねられるため、質問の負担が少なくなければなりません。線をたどることでそれらに答えると、ドキュメント全体の描画にコストがかかります。このファイルはそれを避けるために存在します。そのため、合計は 2 つのプレフィックス ツリーに保持されます。1 つは測定された高さ、もう 1 つは測定された行数です。測定しないものはそれぞれ「単位」であり、その数は引き算です。

/// 何も測定される前に価値があるために行が取られます。
const GUESS: f64 = 20.0;

/// 未測定の行の見積もりを凍結するまでに測る行数。
const SETTLE: f64 = 512.0;

pub(super) struct Heights {
    /// 各行の測定された高さ。 '0.0' は一度も描かれていない線を表します。
    each: Vec<f64>,
    /// 「each」のプレフィックスの合計と、測定される行数。
    sum: Tree,
    seen: Tree,
    /// 凍結された見積もり。測るたびに平均が動くと、その微差 × 未測定の行数の
    /// ぶんだけ場所取りが伸び縮みし、スクロールが測定のたびに流れてしまう。
    /// 十分に測ったら見積もりを固定して、置き場所を安定させる。
    settled: std::cell::Cell<Option<f64>>,
}

impl Heights {
    pub(super) fn new() -> Self {
        Self {
            each: Vec::new(),
            sum: Tree::new(0),
            seen: Tree::new(0),
            settled: std::cell::Cell::new(None),
        }
    }

    /// 1 行につき 1 つの高さを維持します。測定値は、そのラインが画面外で費やした時間よりも長く存続します。途中で追加または削除された線はその後の線に移動し、再度描画されるまでその高さは失われます。
    pub(super) fn fit(&mut self, count: usize) {
        if self.each.len() == count {
            return;
        }
        self.each.resize(count, 0.0);
        self.rebuild();
    }

    pub(super) fn set(&mut self, line: usize, height: f64) {
        let Some(slot) = self.each.get_mut(line) else {
            return;
        };
        let was = *slot;
        if (was - height).abs() < 0.01 {
            return;
        }
        *slot = height;
        self.sum.add(line, height - was);
        if was <= 0.0 {
            self.seen.add(line, 1.0);
        }
    }

    /// 測定されていない線の価値は、測定された線の平均値とみなされるため、長い線の文書は短い線として推測されません。
    fn unit(&self) -> f64 {
        if let Some(settled) = self.settled.get() {
            return settled;
        }
        let seen = self.seen.total();
        if seen <= 0.0 {
            return GUESS;
        }
        let unit = self.sum.total() / seen;
        if seen >= SETTLE {
            self.settled.set(Some(unit));
        }
        unit
    }

    /// 最初の「行」の行を合わせた高さ。
    fn upto(&self, lines: usize) -> f64 {
        let lines = lines.min(self.each.len());
        let measured = self.seen.upto(lines);
        self.sum.upto(lines) + (lines as f64 - measured) * self.unit()
    }

    /// 文書の上部から測った線の開始位置。
    pub(super) fn top_of(&self, line: usize) -> f64 {
        self.upto(line)
    }

    /// 一連の線の高さ。
    pub(super) fn span(&self, lines: std::ops::Range<usize>) -> f64 {
        (self.upto(lines.end) - self.upto(lines.start)).max(0.0)
    }

    /// 最初の行は「y」まで続き、そこから描画が始まります。
    pub(super) fn line_at(&self, y: f64) -> usize {
        let count = self.each.len();
        if y <= 0.0 || count == 0 {
            return 0;
        }
        // 原稿は下に向かってしか伸びないので、奥の線は歩くのではなく半分に切ることで探すことができます。
        let (mut low, mut high) = (0, count - 1);
        while low < high {
            let middle = (low + high) / 2;
            if self.upto(middle + 1) <= y {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        low
    }

    fn rebuild(&mut self) {
        self.sum = Tree::new(self.each.len());
        self.seen = Tree::new(self.each.len());
        // 行数が変わった（別の文書か大きな編集）ので、見積もりも取り直す。
        self.settled.set(None);
        for (line, height) in self.each.iter().enumerate() {
            if *height > 0.0 {
                self.sum.add(line, *height);
                self.seen.add(line, 1.0);
            }
        }
    }
}

/// 両方とも対数時間で、一度に 1 か所ずつ変更できるプレフィックスの合計 (フェンウィック ツリー)。単純な現在までの合計は、測定ごとに再度合計する必要があります。
struct Tree {
    slots: Vec<f64>,
}

impl Tree {
    fn new(len: usize) -> Self {
        Self {
            slots: vec![0.0; len + 1],
        }
    }

    fn add(&mut self, index: usize, delta: f64) {
        let mut at = index + 1;
        while at < self.slots.len() {
            self.slots[at] += delta;
            at += at & at.wrapping_neg();
        }
    }

    /// 最初の「count」桁の合計。
    fn upto(&self, count: usize) -> f64 {
        let mut at = count.min(self.slots.len() - 1);
        let mut total = 0.0;
        while at > 0 {
            total += self.slots[at];
            at -= at & at.wrapping_neg();
        }
        total
    }

    fn total(&self) -> f64 {
        self.upto(self.slots.len() - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guesses_unmeasured_lines_by_what_the_others_measured() {
        let mut heights = Heights::new();
        heights.fit(4);
        assert_eq!(heights.span(0..4), GUESS * 4.0);
        heights.set(0, 30.0);
        heights.set(1, 30.0);
        // 測定されていない 2 つの線は、測定された線と同じ価値があります。
        assert_eq!(heights.span(0..4), 120.0);
        assert_eq!(heights.top_of(2), 60.0);
    }

    #[test]
    fn finds_the_line_at_a_depth() {
        let mut heights = Heights::new();
        heights.fit(5);
        for line in 0..5 {
            heights.set(line, 10.0);
        }
        assert_eq!(heights.line_at(0.0), 0);
        assert_eq!(heights.line_at(9.0), 0);
        assert_eq!(heights.line_at(10.0), 1);
        assert_eq!(heights.line_at(45.0), 4);
        // 終わりを越えたところは最後の行であり、そこにない行ではありません。
        assert_eq!(heights.line_at(1000.0), 4);
    }

    #[test]
    fn measuring_a_line_again_replaces_its_height() {
        let mut heights = Heights::new();
        heights.fit(2);
        heights.set(0, 10.0);
        heights.set(0, 40.0);
        assert_eq!(heights.span(0..1), 40.0);
        // 1 つの線が 40 で測定されると、もう 1 つの線も同様に 40 の価値があります。
        assert_eq!(heights.span(0..2), 80.0);
    }
}
