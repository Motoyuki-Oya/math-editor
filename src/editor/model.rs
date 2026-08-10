//! The document the editor owns, kept independent of the DOM.
//!
//! A line is a sequence of [`Item`]s, so an island counts as one item and the
//! caret can never land inside it by accident. Editing goes through [`Editor`],
//! which holds one or more selections and applies every command to all of them
//! as a single step, the way multiple cursors are expected to behave.

use crate::doc::{self, Segment};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    Char(char),
    /// An island, kept as the notation between its `$(` and `)`.
    Math {
        source: String,
    },
}

impl Item {
    pub fn as_char(&self) -> Option<char> {
        match self {
            Item::Char(c) => Some(*c),
            Item::Math { .. } => None,
        }
    }
}

/// A place between two items. `col` counts items, not bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

impl Pos {
    pub fn new(line: usize, col: usize) -> Self {
        Self { line, col }
    }
}

/// A caret (`anchor == head`) or a selected range, growing from `anchor`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sel {
    pub anchor: Pos,
    pub head: Pos,
}

impl Sel {
    pub fn caret(at: Pos) -> Self {
        Self {
            anchor: at,
            head: at,
        }
    }

    pub fn range(from: Pos, to: Pos) -> Self {
        Self {
            anchor: from,
            head: to,
        }
    }

    pub fn start(&self) -> Pos {
        self.anchor.min(self.head)
    }

    pub fn end(&self) -> Pos {
        self.anchor.max(self.head)
    }

    pub fn is_caret(&self) -> bool {
        self.anchor == self.head
    }
}

/// The lines of the document. There is always at least one line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Text {
    lines: Vec<Vec<Item>>,
}

impl Default for Text {
    fn default() -> Self {
        Self {
            lines: vec![Vec::new()],
        }
    }
}

impl Text {
    pub fn from_document(source: &str) -> Self {
        let lines = doc::parse(source)
            .into_iter()
            .map(|line| {
                line.into_iter()
                    .flat_map(|segment| match segment {
                        Segment::Text(text) => text.chars().map(Item::Char).collect::<Vec<_>>(),
                        Segment::Island(source) => vec![Item::Math { source }],
                    })
                    .collect()
            })
            .collect::<Vec<Vec<Item>>>();
        Self {
            lines: if lines.is_empty() {
                vec![Vec::new()]
            } else {
                lines
            },
        }
    }

    pub fn to_document(&self) -> String {
        let lines: Vec<doc::Line> = self
            .lines
            .iter()
            .map(|items| {
                let mut segments = doc::Line::new();
                let mut text = String::new();
                for item in items {
                    match item {
                        Item::Char(c) => text.push(*c),
                        Item::Math { source } => {
                            if !text.is_empty() {
                                segments.push(Segment::Text(std::mem::take(&mut text)));
                            }
                            segments.push(Segment::Island(source.clone()));
                        }
                    }
                }
                if !text.is_empty() {
                    segments.push(Segment::Text(text));
                }
                segments
            })
            .collect();
        doc::serialize(&lines)
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn line(&self, line: usize) -> &[Item] {
        self.lines.get(line).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn line_len(&self, line: usize) -> usize {
        self.line(line).len()
    }

    pub fn item_at(&self, at: Pos) -> Option<&Item> {
        self.line(at.line).get(at.col)
    }

    pub fn end(&self) -> Pos {
        let line = self.line_count() - 1;
        Pos::new(line, self.line_len(line))
    }

    pub fn clamp(&self, at: Pos) -> Pos {
        let line = at.line.min(self.line_count() - 1);
        Pos::new(line, at.col.min(self.line_len(line)))
    }

    pub fn slice(&self, from: Pos, to: Pos) -> Vec<Vec<Item>> {
        let (from, to) = (self.clamp(from), self.clamp(to));
        if from.line == to.line {
            return vec![self.line(from.line)[from.col..to.col].to_vec()];
        }
        let mut out = vec![self.line(from.line)[from.col..].to_vec()];
        for line in from.line + 1..to.line {
            out.push(self.line(line).to_vec());
        }
        out.push(self.line(to.line)[..to.col].to_vec());
        out
    }

    /// Removes everything between the two places and returns where they joined.
    pub fn remove(&mut self, from: Pos, to: Pos) -> Pos {
        let (from, to) = (self.clamp(from), self.clamp(to));
        if from == to {
            return from;
        }
        let tail = self.lines[to.line][to.col..].to_vec();
        self.lines[from.line].truncate(from.col);
        self.lines[from.line].extend(tail);
        self.lines.drain(from.line + 1..=to.line);
        from
    }

    /// Inserts lines of items and returns the place just after them.
    pub fn insert(&mut self, at: Pos, mut what: Vec<Vec<Item>>) -> Pos {
        let at = self.clamp(at);
        if what.is_empty() {
            return at;
        }
        let tail = self.lines[at.line][at.col..].to_vec();
        self.lines[at.line].truncate(at.col);
        if what.len() == 1 {
            let only = what.remove(0);
            let col = at.col + only.len();
            self.lines[at.line].extend(only);
            self.lines[at.line].extend(tail);
            return Pos::new(at.line, col);
        }
        let last = what.pop().expect("more than one line");
        let first = what.remove(0);
        self.lines[at.line].extend(first);
        let end = Pos::new(at.line + what.len() + 1, last.len());
        let mut rest: Vec<Vec<Item>> = what;
        let mut last_line = last;
        last_line.extend(tail);
        rest.push(last_line);
        for (offset, line) in rest.into_iter().enumerate() {
            self.lines.insert(at.line + 1 + offset, line);
        }
        end
    }

    pub fn set_math(&mut self, at: Pos, source: String) -> bool {
        match self
            .lines
            .get_mut(at.line)
            .and_then(|line| line.get_mut(at.col))
        {
            Some(Item::Math { source: slot }) => {
                *slot = source;
                true
            }
            _ => false,
        }
    }

    /// Characters and lines, for the status bar. A formula counts as one.
    pub fn stats(&self) -> (usize, usize) {
        (self.lines.iter().map(Vec::len).sum(), self.line_count())
    }
}

/// Where `pos` ends up once the text up to `to` has been replaced by text that
/// now ends at `end`.
fn shifted(pos: Pos, to: Pos, end: Pos) -> Pos {
    if pos <= to {
        // Anything the edit swallowed sits at its end.
        return end;
    }
    let line = (pos.line + end.line).saturating_sub(to.line);
    if pos.line == to.line {
        Pos::new(line, pos.col - to.col + end.col)
    } else {
        Pos::new(line, pos.col)
    }
}

/// The place one item to the left on the same line, if there is one.
pub fn before_col(at: Pos) -> Option<Pos> {
    (at.col > 0).then(|| Pos::new(at.line, at.col - 1))
}

/// The place one item to the left, used after inserting an item to point at it.
pub fn before_pos(at: Pos) -> Pos {
    before_col(at).unwrap_or(at)
}

pub fn items_of(text: &str) -> Vec<Vec<Item>> {
    text.split('\n')
        .map(|line| line.chars().map(Item::Char).collect())
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Typed characters, which join with the step before them.
    Typing,
    Other,
}

#[derive(Clone)]
struct Snapshot {
    text: Text,
    sels: Vec<Sel>,
}

pub struct Editor {
    text: Text,
    sels: Vec<Sel>,
    past: Vec<Snapshot>,
    future: Vec<Snapshot>,
    last: Step,
}

const HISTORY_LIMIT: usize = 500;

impl Default for Editor {
    fn default() -> Self {
        Self {
            text: Text::default(),
            sels: vec![Sel::caret(Pos::default())],
            past: Vec::new(),
            future: Vec::new(),
            last: Step::Other,
        }
    }
}

impl Editor {
    pub fn text(&self) -> &Text {
        &self.text
    }

    pub fn sels(&self) -> &[Sel] {
        &self.sels
    }

    /// The selection that keeps the focus; the one added last.
    pub fn primary(&self) -> Sel {
        *self.sels.last().expect("at least one selection")
    }

    pub fn load(&mut self, source: &str) {
        self.text = Text::from_document(source);
        self.sels = vec![Sel::caret(Pos::default())];
        self.past.clear();
        self.future.clear();
        self.last = Step::Other;
    }

    pub fn to_document(&self) -> String {
        self.text.to_document()
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            text: self.text.clone(),
            sels: self.sels.clone(),
        }
    }

    fn record(&mut self, step: Step) {
        let join = step == Step::Typing && self.last == Step::Typing;
        self.last = step;
        self.future.clear();
        if join {
            return;
        }
        self.past.push(self.snapshot());
        if self.past.len() > HISTORY_LIMIT {
            self.past.remove(0);
        }
    }

    pub fn undo(&mut self) -> bool {
        let Some(previous) = self.past.pop() else {
            return false;
        };
        self.future.push(self.snapshot());
        self.text = previous.text;
        self.sels = previous.sels;
        self.last = Step::Other;
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.future.pop() else {
            return false;
        };
        self.past.push(self.snapshot());
        self.text = next.text;
        self.sels = next.sels;
        self.last = Step::Other;
        true
    }

    /// Applies an edit at every selection as one step. `edit` returns the items
    /// to put in place of the selection, together with the range to remove.
    ///
    /// The edits run front to back, and every edit moves the selections that
    /// come after it, so each one still points at the same text afterwards.
    fn edit_each(&mut self, step: Step, edit: impl Fn(&Text, Sel) -> (Pos, Pos, Vec<Vec<Item>>)) {
        self.record(step);
        let mut order: Vec<usize> = (0..self.sels.len()).collect();
        order.sort_by_key(|&i| self.sels[i].start());
        for (done, &i) in order.iter().enumerate() {
            let (from, to, what) = edit(&self.text, self.sels[i]);
            let at = self.text.remove(from, to);
            let end = self.text.insert(at, what);
            self.sels[i] = Sel::caret(end);
            for &later in &order[done + 1..] {
                let sel = self.sels[later];
                self.sels[later] =
                    Sel::range(shifted(sel.anchor, to, end), shifted(sel.head, to, end));
            }
        }
        self.merge_sels();
    }

    pub fn insert(&mut self, what: Vec<Vec<Item>>) {
        let typing = what.len() == 1 && what[0].len() == 1;
        let step = if typing { Step::Typing } else { Step::Other };
        self.edit_each(step, move |_, sel| (sel.start(), sel.end(), what.clone()));
    }

    pub fn insert_text(&mut self, text: &str) {
        self.insert(items_of(text));
    }

    pub fn insert_math(&mut self, source: &str) {
        self.insert(vec![vec![Item::Math {
            source: source.to_string(),
        }]]);
    }

    pub fn split_line(&mut self) {
        self.insert(vec![Vec::new(), Vec::new()]);
    }

    pub fn backspace(&mut self) {
        self.edit_each(Step::Other, |text, sel| {
            if sel.is_caret() {
                (before(text, sel.head), sel.head, Vec::new())
            } else {
                (sel.start(), sel.end(), Vec::new())
            }
        });
    }

    pub fn delete_forward(&mut self) {
        self.edit_each(Step::Other, |text, sel| {
            if sel.is_caret() {
                (sel.head, after(text, sel.head), Vec::new())
            } else {
                (sel.start(), sel.end(), Vec::new())
            }
        });
    }

    /// Replaces one range, used by search and replace.
    pub fn replace_range(&mut self, from: Pos, to: Pos, with: &str) {
        self.record(Step::Other);
        let at = self.text.remove(from, to);
        let end = self.text.insert(at, items_of(with));
        self.sels = vec![Sel::caret(end)];
    }

    /// Starts one undo step for a whole formula editing session, so the keys
    /// pressed inside a formula do not each become a step of their own.
    pub fn begin_math_edit(&mut self) {
        self.record(Step::Other);
    }

    pub fn set_math_at(&mut self, at: Pos, source: &str) {
        self.text.set_math(at, source.to_string());
    }

    pub fn move_h(&mut self, forward: bool, extend: bool) {
        self.map_sels(extend, |text, head| {
            if forward {
                after(text, head)
            } else {
                before(text, head)
            }
        });
    }

    pub fn move_v(&mut self, down: bool, extend: bool) {
        self.map_sels(extend, |text, head| {
            let line = if down {
                head.line + 1
            } else {
                head.line.checked_sub(1).unwrap_or(head.line)
            };
            text.clamp(Pos::new(line.min(text.line_count() - 1), head.col))
        });
    }

    pub fn move_line_edge(&mut self, end: bool, extend: bool) {
        self.map_sels(extend, |text, head| {
            Pos::new(head.line, if end { text.line_len(head.line) } else { 0 })
        });
    }

    pub fn move_document_edge(&mut self, end: bool, extend: bool) {
        self.map_sels(
            extend,
            |text, _| {
                if end {
                    text.end()
                } else {
                    Pos::default()
                }
            },
        );
    }

    fn map_sels(&mut self, extend: bool, step: impl Fn(&Text, Pos) -> Pos) {
        self.last = Step::Other;
        for sel in &mut self.sels {
            // Collapsing a selection without shift keeps the near edge, like
            // every other editor.
            let from = if extend || sel.is_caret() {
                sel.head
            } else {
                sel.head.min(sel.anchor).max(sel.start())
            };
            let head = step(&self.text, from);
            sel.head = head;
            if !extend {
                sel.anchor = head;
            }
        }
        self.merge_sels();
    }

    pub fn set_caret(&mut self, at: Pos) {
        self.last = Step::Other;
        self.sels = vec![Sel::caret(self.text.clamp(at))];
    }

    pub fn extend_to(&mut self, at: Pos) {
        self.last = Step::Other;
        let at = self.text.clamp(at);
        if let Some(sel) = self.sels.last_mut() {
            sel.head = at;
        }
        self.merge_sels();
    }

    pub fn add_caret(&mut self, at: Pos) {
        self.last = Step::Other;
        self.sels.push(Sel::caret(self.text.clamp(at)));
        self.merge_sels();
    }

    pub fn select_all(&mut self) {
        self.last = Step::Other;
        self.sels = vec![Sel::range(Pos::default(), self.text.end())];
    }

    pub fn set_sels(&mut self, sels: Vec<Sel>) {
        self.last = Step::Other;
        if sels.is_empty() {
            return;
        }
        self.sels = sels;
        self.merge_sels();
    }

    /// Drops the extra cursors, keeping the one that has the focus.
    pub fn collapse_sels(&mut self) -> bool {
        if self.sels.len() == 1 {
            return false;
        }
        self.sels = vec![self.primary()];
        true
    }

    /// `Ctrl+D`: selects the word at the caret, then each further press adds the
    /// next place where the same text appears.
    pub fn add_next_occurrence(&mut self) -> bool {
        self.last = Step::Other;
        let primary = self.primary();
        if primary.is_caret() {
            let Some(word) = word_at(&self.text, primary.head) else {
                return false;
            };
            *self.sels.last_mut().expect("a selection") = word;
            return true;
        }
        let needle: Vec<Item> = self
            .text
            .slice(primary.start(), primary.end())
            .into_iter()
            .next()
            .unwrap_or_default();
        if needle.is_empty() || primary.start().line != primary.end().line {
            return false;
        }
        let taken: Vec<Pos> = self.sels.iter().map(Sel::start).collect();
        let Some(found) = find_after(&self.text, &needle, primary.end(), &taken) else {
            return false;
        };
        self.sels.push(found);
        true
    }

    /// Keeps the selections sorted and free of overlap, so typing never applies
    /// the same edit twice.
    fn merge_sels(&mut self) {
        let primary = self.primary();
        self.sels.sort_by_key(|sel| (sel.start(), sel.end()));
        let mut merged: Vec<Sel> = Vec::with_capacity(self.sels.len());
        for sel in std::mem::take(&mut self.sels) {
            match merged.last_mut() {
                Some(last) if sel.start() <= last.end() => {
                    if sel.end() > last.end() {
                        *last = Sel::range(last.start(), sel.end());
                    }
                }
                _ => merged.push(sel),
            }
        }
        // The focused selection must stay last so `primary` keeps meaning it.
        if let Some(index) = merged
            .iter()
            .position(|sel| sel.start() <= primary.start() && primary.end() <= sel.end())
        {
            let focused = merged.remove(index);
            merged.push(focused);
        }
        self.sels = merged;
    }
}

fn before(text: &Text, at: Pos) -> Pos {
    if at.col > 0 {
        Pos::new(at.line, at.col - 1)
    } else if at.line > 0 {
        Pos::new(at.line - 1, text.line_len(at.line - 1))
    } else {
        at
    }
}

fn after(text: &Text, at: Pos) -> Pos {
    if at.col < text.line_len(at.line) {
        Pos::new(at.line, at.col + 1)
    } else if at.line + 1 < text.line_count() {
        Pos::new(at.line + 1, 0)
    } else {
        at
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn word_at(text: &Text, at: Pos) -> Option<Sel> {
    let line = text.line(at.line);
    let word = |col: usize| line.get(col).and_then(Item::as_char).is_some_and(is_word);
    let mut start = at.col;
    while start > 0 && word(start - 1) {
        start -= 1;
    }
    let mut end = at.col;
    while word(end) {
        end += 1;
    }
    (start < end).then(|| Sel::range(Pos::new(at.line, start), Pos::new(at.line, end)))
}

/// Looks for the items after `from`, wrapping around, skipping places that are
/// already selected.
fn find_after(text: &Text, needle: &[Item], from: Pos, taken: &[Pos]) -> Option<Sel> {
    let mut at = from;
    for _ in 0..text.line_count() + 1 {
        for line in at.line..text.line_count() {
            let items = text.line(line);
            let start_col = if line == at.line { at.col } else { 0 };
            for col in start_col..=items.len().saturating_sub(needle.len()) {
                if items.len() < needle.len() {
                    break;
                }
                if &items[col..col + needle.len()] == needle
                    && !taken.contains(&Pos::new(line, col))
                {
                    return Some(Sel::range(
                        Pos::new(line, col),
                        Pos::new(line, col + needle.len()),
                    ));
                }
            }
        }
        if at == Pos::default() {
            return None;
        }
        at = Pos::default();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor(source: &str) -> Editor {
        let mut editor = Editor::default();
        editor.load(source);
        editor
    }

    /// The text of the document, with each island standing in as one character.
    fn plain(editor: &Editor) -> String {
        (0..editor.text().line_count())
            .map(|line| {
                editor
                    .text()
                    .line(line)
                    .iter()
                    .map(|item| item.as_char().unwrap_or('\u{fffc}'))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn islands_are_one_item() {
        let editor = editor("a $(1/2) b");
        assert_eq!(editor.text().line_len(0), 5);
        assert!(matches!(
            editor.text().item_at(Pos::new(0, 2)),
            Some(Item::Math { .. })
        ));
        assert_eq!(editor.to_document(), "a $(1/2) b");
    }

    #[test]
    fn typing_at_two_cursors_edits_both() {
        let mut editor = editor("ab\nab");
        editor.set_caret(Pos::new(0, 1));
        editor.add_caret(Pos::new(1, 1));
        editor.insert_text("X");
        assert_eq!(plain(&editor), "aXb\naXb");
        assert_eq!(editor.sels().len(), 2);
        assert_eq!(editor.sels()[0].head, Pos::new(0, 2));
    }

    #[test]
    fn cursors_on_one_line_stay_on_their_own_text() {
        let mut editor = editor("AAA BBB");
        editor.set_caret(Pos::new(0, 3));
        editor.add_caret(Pos::new(0, 7));
        editor.insert_text("X");
        assert_eq!(plain(&editor), "AAAX BBBX");
        // Each caret must delete the character it just typed.
        editor.backspace();
        assert_eq!(plain(&editor), "AAA BBB");
    }

    #[test]
    fn a_newline_at_an_earlier_cursor_moves_the_later_one() {
        let mut editor = editor("ab cd");
        editor.set_caret(Pos::new(0, 2));
        editor.add_caret(Pos::new(0, 5));
        editor.split_line();
        assert_eq!(plain(&editor), "ab\n cd\n");
        assert_eq!(editor.sels()[1].head, Pos::new(2, 0));
    }

    #[test]
    fn undo_takes_back_a_whole_multi_cursor_edit() {
        let mut editor = editor("ab\nab");
        editor.set_caret(Pos::new(0, 1));
        editor.add_caret(Pos::new(1, 1));
        editor.insert_text("X");
        editor.insert_text("Y");
        assert_eq!(plain(&editor), "aXYb\naXYb");
        // Typing joins into one step, so one undo clears both characters.
        assert!(editor.undo());
        assert_eq!(plain(&editor), "ab\nab");
        assert!(editor.redo());
        assert_eq!(plain(&editor), "aXYb\naXYb");
    }

    #[test]
    fn backspace_joins_lines_and_deletes_an_island_whole() {
        let mut editor = editor("a$(x)\nb");
        editor.set_caret(Pos::new(1, 0));
        editor.backspace();
        assert_eq!(plain(&editor), "a\u{fffc}b");
        editor.set_caret(Pos::new(0, 2));
        editor.backspace();
        assert_eq!(editor.to_document(), "ab");
    }

    #[test]
    fn enter_splits_and_undo_restores() {
        let mut editor = editor("ab");
        editor.set_caret(Pos::new(0, 1));
        editor.split_line();
        assert_eq!(plain(&editor), "a\nb");
        assert_eq!(editor.primary().head, Pos::new(1, 0));
        assert!(editor.undo());
        assert_eq!(plain(&editor), "ab");
    }

    #[test]
    fn ctrl_d_selects_the_word_then_the_next_one() {
        let mut editor = editor("foo bar\nfoo");
        editor.set_caret(Pos::new(0, 1));
        assert!(editor.add_next_occurrence());
        assert_eq!(editor.primary(), Sel::range(Pos::new(0, 0), Pos::new(0, 3)));
        assert!(editor.add_next_occurrence());
        assert_eq!(editor.sels().len(), 2);
        assert_eq!(editor.primary(), Sel::range(Pos::new(1, 0), Pos::new(1, 3)));
        editor.insert_text("qux");
        assert_eq!(plain(&editor), "qux bar\nqux");
    }

    #[test]
    fn overlapping_cursors_collapse_into_one() {
        let mut editor = editor("abc");
        editor.set_caret(Pos::new(0, 1));
        editor.add_caret(Pos::new(0, 1));
        assert_eq!(editor.sels().len(), 1);
    }

    #[test]
    fn moving_down_keeps_the_column_within_the_line() {
        let mut editor = editor("long line\nab");
        editor.set_caret(Pos::new(0, 9));
        editor.move_v(true, false);
        assert_eq!(editor.primary().head, Pos::new(1, 2));
    }

    #[test]
    fn selecting_then_typing_replaces_the_selection() {
        let mut editor = editor("hello");
        editor.set_caret(Pos::new(0, 0));
        editor.extend_to(Pos::new(0, 5));
        editor.insert_text("bye");
        assert_eq!(plain(&editor), "bye");
    }

    #[test]
    fn the_notation_round_trips_through_the_model() {
        let source = "text a$(^ 2) more\n$(1/2)\nend";
        assert_eq!(editor(source).to_document(), source);
    }
}
