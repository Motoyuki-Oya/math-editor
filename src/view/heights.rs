//! How tall every line is, and where a line therefore sits.
//!
//! Only the lines that can be seen are drawn, so which lines those are has to
//! be worked out from heights rather than from the page. A line that has been
//! drawn has a measured height; a line never drawn is taken to be worth what
//! the measured ones are on average.
//!
//! Both questions — "how tall are lines `a..b`" and "which line is at `y`" —
//! have to be cheap, because they are asked on every scroll of a document that
//! may have millions of lines. Answering them by walking the lines would make
//! drawing cost the whole document, which is what this file exists to avoid, so
//! the sums are kept in two prefix trees: one of measured heights, one of how
//! many lines have been measured. What is not measured is `unit` each, and the
//! number of those is a subtraction.

/// What a line is taken to be worth before anything has been measured.
const GUESS: f64 = 20.0;

pub(super) struct Heights {
    /// Every line's height as measured; `0.0` for a line never drawn.
    each: Vec<f64>,
    /// Prefix sums of `each`, and of how many lines are measured.
    sum: Tree,
    seen: Tree,
}

impl Heights {
    pub(super) fn new() -> Self {
        Self {
            each: Vec::new(),
            sum: Tree::new(0),
            seen: Tree::new(0),
        }
    }

    /// Keeps one height per line. A measurement outlives the time its line
    /// spends off screen; lines added or removed in the middle shift the ones
    /// after them, whose heights are then stale until they are drawn again.
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

    /// What an unmeasured line is taken to be worth: what the measured ones
    /// are, on average, so a document of tall lines is not guessed at as short
    /// ones.
    fn unit(&self) -> f64 {
        let seen = self.seen.total();
        if seen <= 0.0 {
            return GUESS;
        }
        self.sum.total() / seen
    }

    /// How tall the first `lines` lines are together.
    fn upto(&self, lines: usize) -> f64 {
        let lines = lines.min(self.each.len());
        let measured = self.seen.upto(lines);
        self.sum.upto(lines) + (lines as f64 - measured) * self.unit()
    }

    /// Where a line starts, measured from the top of the document.
    pub(super) fn top_of(&self, line: usize) -> f64 {
        self.upto(line)
    }

    /// How tall a stretch of lines is.
    pub(super) fn span(&self, lines: std::ops::Range<usize>) -> f64 {
        (self.upto(lines.end) - self.upto(lines.start)).max(0.0)
    }

    /// The first line reaching down to `y`, which is where drawing starts.
    pub(super) fn line_at(&self, y: f64) -> usize {
        let count = self.each.len();
        if y <= 0.0 || count == 0 {
            return 0;
        }
        // The document only grows downwards, so the line at a depth can be
        // looked for by halving rather than by walking.
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
        for (line, height) in self.each.iter().enumerate() {
            if *height > 0.0 {
                self.sum.add(line, *height);
                self.seen.add(line, 1.0);
            }
        }
    }
}

/// Prefix sums that can be changed one place at a time, both in logarithmic
/// time (a Fenwick tree). A plain running total would have to be added up again
/// on every measurement.
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

    /// The sum of the first `count` places.
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
        // The two unmeasured lines are worth what the measured ones are.
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
        // Past the end is the last line, not a line that is not there.
        assert_eq!(heights.line_at(1000.0), 4);
    }

    #[test]
    fn measuring_a_line_again_replaces_its_height() {
        let mut heights = Heights::new();
        heights.fit(2);
        heights.set(0, 10.0);
        heights.set(0, 40.0);
        assert_eq!(heights.span(0..1), 40.0);
        // One line measured at 40 makes the other one worth 40 as well.
        assert_eq!(heights.span(0..2), 80.0);
    }
}
