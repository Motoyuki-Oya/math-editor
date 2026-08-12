//! Editing a document: the selections, the commands they apply, and the undo
//! history.
//!
//! Every command applies to all of the selections as a single step, the way
//! multiple cursors are expected to behave. The document itself is
//! [`crate::structure::text`], which knows nothing about the notation or the
//! screen.

use crate::structure::ast::{row_at, Cursor, Node, Row};
use crate::structure::edit::{Editing, Escape};
pub use crate::structure::text::{before_col, before_pos, items_of, Item, Pos, Sel, Text};

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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Typed characters, which join with the step before them.
    Typing,
    Other,
}

/// What a command inside an island does to the document, which decides both
/// how it joins the undo history and whether the file became dirty.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Inside {
    /// Only the caret moved.
    Move,
    /// The selection grew or shrank; the anchor stays where it is.
    Extend,
    /// A character was typed, joining the step before it.
    Type,
    /// The structure changed some other way.
    Change,
}

#[derive(Clone)]
struct Snapshot {
    text: Text,
    sels: Vec<Sel>,
    inside: Option<Cursor>,
}

pub struct Editor {
    text: Text,
    sels: Vec<Sel>,
    /// Where the caret is inside the island it stands on, when it is in one.
    /// The caret is one place in one document either way: this only says how
    /// deep into that place it reaches.
    inside: Option<Cursor>,
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
            inside: None,
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

    /// Shows a document that was just read from a file, dropping the history.
    pub fn load(&mut self, text: Text) {
        self.text = text;
        self.sels = vec![Sel::caret(Pos::default())];
        self.inside = None;
        self.past.clear();
        self.future.clear();
        self.last = Step::Other;
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            text: self.text.clone(),
            sels: self.sels.clone(),
            inside: self.inside.clone(),
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
        self.inside = previous.inside;
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
        self.inside = next.inside;
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
        self.inside = None;
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

    pub fn insert_math(&mut self, row: Row) {
        self.insert(vec![vec![Item::Math(row)]]);
    }

    pub fn insert_tab(&mut self) {
        self.insert(vec![vec![Item::Tab]]);
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
        self.inside = None;
        let at = self.text.remove(from, to);
        let end = self.text.insert(at, items_of(with));
        self.sels = vec![Sel::caret(end)];
    }

    /// Where the caret is inside the island it stands on, if it is in one.
    pub fn inside(&self) -> Option<&Cursor> {
        self.inside.as_ref()
    }

    /// Puts an empty island at the caret and steps into it.
    pub fn insert_island(&mut self) {
        self.insert_math(Row::new());
        let at = before_pos(self.primary().head);
        self.sels = vec![Sel::caret(at)];
        self.inside = Some(Cursor::root(0));
        // Starting a formula is a step of its own, so undoing what was typed
        // into it does not take the formula away as well.
        self.last = Step::Other;
    }

    /// Steps into the island at `at`, from the left or from the right.
    pub fn enter_island(&mut self, at: Pos, from_start: bool) -> bool {
        if !matches!(self.text.item_at(at), Some(Item::Math(_))) {
            return false;
        }
        self.sels = vec![Sel::caret(at)];
        self.inside = Some(Cursor::default());
        self.in_island(Inside::Move, |editing| {
            if from_start {
                editing.move_to_start();
            } else {
                editing.move_to_end();
            }
            None
        })
    }

    /// Steps back out of the island, leaving the caret beside it.
    pub fn leave_island(&mut self) -> bool {
        if self.inside.take().is_none() {
            return false;
        }
        let at = self.primary().head;
        self.last = Step::Other;
        self.sels = vec![Sel::caret(self.text.clamp(Pos::new(at.line, at.col + 1)))];
        true
    }

    /// Runs a command on the island the caret is in. The island is edited where
    /// it lives, so the command is a step of the document's own history.
    pub fn in_island(
        &mut self,
        kind: Inside,
        command: impl FnOnce(&mut Editing<'_>) -> Option<Escape>,
    ) -> bool {
        // The cursor is copied out, not taken: the history is written before the
        // command runs, and it has to remember where the caret was inside.
        let Some(mut cursor) = self.inside.clone() else {
            return false;
        };
        let at = self.primary().head;
        match kind {
            Inside::Move | Inside::Extend => self.last = Step::Other,
            Inside::Type => self.record(Step::Typing),
            Inside::Change => self.record(Step::Other),
        }
        let Some(root) = self.text.math_at_mut(at) else {
            return false;
        };
        let escape = command(&mut Editing::new(root, &mut cursor));
        self.inside = Some(cursor);
        match escape {
            // A selection that outgrew the formula becomes a selection of the
            // formula, which is one item of the text like any other.
            Some(_) if kind == Inside::Extend => {
                self.inside = None;
                let after = self.text.clamp(Pos::new(at.line, at.col + 1));
                self.sels = vec![Sel::range(at, after)];
                true
            }
            Some(escape) => {
                self.escape_island(at, escape, !matches!(kind, Inside::Move | Inside::Extend))
            }
            None => true,
        }
    }

    /// The structures the selection inside a formula covers, for a copy.
    pub fn island_selection(&self) -> Option<Row> {
        let cursor = self.inside.as_ref()?;
        if cursor.is_caret() {
            return None;
        }
        let Some(Item::Math(root)) = self.text.item_at(self.primary().head) else {
            return None;
        };
        let row = row_at(root, &cursor.path)?;
        Some(row[cursor.start()..cursor.end().min(row.len())].to_vec())
    }

    pub fn insert_in_island(&mut self, node: Node) -> bool {
        self.in_island(Inside::Change, |editing| {
            editing.insert(node);
            None
        })
    }

    pub fn insert_row_in_island(&mut self, nodes: Row) -> bool {
        self.in_island(Inside::Change, |editing| {
            editing.insert_row(nodes);
            None
        })
    }

    pub fn type_in_island(&mut self, c: char) -> bool {
        self.in_island(Inside::Type, |editing| {
            editing.insert_char(c);
            None
        })
    }

    /// Leaves an island the caret walked out of, taking an empty one with it:
    /// backspacing out of the front of a formula that has nothing left in it
    /// removes the formula.
    fn escape_island(&mut self, at: Pos, escape: Escape, recorded: bool) -> bool {
        let empty = matches!(self.text.item_at(at), Some(Item::Math(row)) if row.is_empty());
        self.inside = None;
        let after = self.text.clamp(Pos::new(at.line, at.col + 1));
        match escape {
            Escape::Delete | Escape::Left if empty => {
                if !recorded {
                    self.record(Step::Other);
                }
                self.text.remove(at, after);
                self.sels = vec![Sel::caret(at)];
            }
            Escape::Left => self.sels = vec![Sel::caret(at)],
            _ => self.sels = vec![Sel::caret(after)],
        }
        true
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
        self.inside = None;
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
        self.inside = None;
        self.sels = vec![Sel::caret(self.text.clamp(at))];
    }

    pub fn extend_to(&mut self, at: Pos) {
        self.last = Step::Other;
        self.inside = None;
        let at = self.text.clamp(at);
        if let Some(sel) = self.sels.last_mut() {
            sel.head = at;
        }
        self.merge_sels();
    }

    pub fn add_caret(&mut self, at: Pos) {
        self.last = Step::Other;
        self.inside = None;
        self.sels.push(Sel::caret(self.text.clamp(at)));
        self.merge_sels();
    }

    pub fn select_all(&mut self) {
        self.last = Step::Other;
        self.inside = None;
        self.sels = vec![Sel::range(Pos::default(), self.text.end())];
    }

    pub fn set_sels(&mut self, sels: Vec<Sel>) {
        self.last = Step::Other;
        self.inside = None;
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

    /// Nothing here goes through the notation: the model is only ever handed
    /// items, so these tests cannot start depending on the file format.
    fn editor(source: &str) -> Editor {
        with_items(items_of(source))
    }

    fn with_items(lines: Vec<Vec<Item>>) -> Editor {
        let mut editor = Editor::default();
        editor.load(Text::from_lines(lines));
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
    fn a_separator_is_one_item() {
        let mut editor = editor("x= 1");
        editor.set_caret(Pos::new(0, 1));
        editor.insert_tab();
        assert_eq!(editor.text().item_at(Pos::new(0, 1)), Some(&Item::Tab));
        assert_eq!(editor.text().line_len(0), 5);
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
        let island = Item::Math(vec![crate::structure::ast::Node::Char('x')]);
        let mut editor = with_items(vec![vec![Item::Char('a'), island], vec![Item::Char('b')]]);
        editor.set_caret(Pos::new(1, 0));
        editor.backspace();
        assert_eq!(plain(&editor), "a\u{fffc}b");
        editor.set_caret(Pos::new(0, 2));
        editor.backspace();
        assert_eq!(plain(&editor), "ab");
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

    /// The island the caret stands on, as its structure.
    fn island(editor: &Editor) -> Row {
        match editor.text().item_at(editor.primary().head) {
            Some(Item::Math(row)) => row.clone(),
            other => panic!("expected an island, got {other:?}"),
        }
    }

    fn started_in_an_island() -> Editor {
        let mut editor = editor("a");
        editor.set_caret(Pos::new(0, 1));
        editor.insert_island();
        editor
    }

    #[test]
    fn a_formula_is_typed_into_the_document_itself() {
        let mut editor = started_in_an_island();
        editor.type_in_island('x');
        assert_eq!(island(&editor), vec![Node::Char('x')]);
        assert_eq!(plain(&editor), "a\u{fffc}");
    }

    /// Typing inside a formula is a step of the document's history, so one undo
    /// takes it back and the caret returns into the formula.
    #[test]
    fn undo_takes_back_typing_inside_a_formula() {
        let mut editor = started_in_an_island();
        editor.type_in_island('x');
        editor.type_in_island('y');
        assert!(editor.undo());
        assert_eq!(island(&editor), Vec::new());
        assert!(editor.inside().is_some());
        assert!(editor.redo());
        assert_eq!(island(&editor).len(), 2);
    }

    /// The caret moves into the lower row of the fraction it just made, which is
    /// the empty slot the next character belongs in.
    #[test]
    fn the_caret_lands_in_the_empty_slot_of_a_new_fraction() {
        let mut editor = started_in_an_island();
        editor.type_in_island('1');
        editor.type_in_island('/');
        let cursor = editor.inside().expect("inside the formula");
        assert_eq!(cursor.path, vec![(0, 1)]);
        assert_eq!(cursor.index, 0);
        editor.type_in_island('2');
        assert_eq!(editor.inside().expect("inside the formula").index, 1);
    }

    #[test]
    fn selecting_inside_a_formula_takes_the_structure_it_reaches() {
        let mut editor = started_in_an_island();
        for c in "1/2".chars() {
            editor.type_in_island(c);
        }
        // Inside the lower row: selecting the `2`, then the fraction itself.
        editor.in_island(Inside::Extend, |editing| editing.extend(false));
        assert_eq!(editor.island_selection(), Some(vec![Node::Char('2')]));
        editor.in_island(Inside::Extend, |editing| editing.extend(false));
        let selected = editor.island_selection().expect("a selection");
        assert!(matches!(selected.as_slice(), [Node::Stack { .. }]));
    }

    /// Once a selection has taken everything in the formula, it becomes a
    /// selection of the formula: one item of the text, like a character.
    #[test]
    fn a_selection_that_outgrows_a_formula_selects_the_formula() {
        let mut editor = started_in_an_island();
        editor.type_in_island('x');
        editor.in_island(Inside::Extend, |editing| editing.extend(false));
        editor.in_island(Inside::Extend, |editing| editing.extend(false));
        assert!(editor.inside().is_none());
        assert_eq!(editor.primary(), Sel::range(Pos::new(0, 1), Pos::new(0, 2)));
    }

    #[test]
    fn typing_over_a_selection_inside_a_formula_replaces_it() {
        let mut editor = started_in_an_island();
        for c in "xy".chars() {
            editor.type_in_island(c);
        }
        editor.in_island(Inside::Extend, |editing| editing.extend(false));
        editor.type_in_island('z');
        assert_eq!(island(&editor), vec![Node::Char('x'), Node::Char('z')]);
    }

    #[test]
    fn moving_off_a_selection_inside_a_formula_leaves_a_caret() {
        let mut editor = started_in_an_island();
        for c in "xy".chars() {
            editor.type_in_island(c);
        }
        editor.in_island(Inside::Extend, |editing| editing.extend(false));
        editor.in_island(Inside::Move, |editing| editing.move_left());
        let cursor = editor.inside().expect("inside the formula");
        assert!(cursor.is_caret());
        assert_eq!(cursor.index, 1);
    }

    /// Walking out of the front of a formula that has nothing left in it takes
    /// the formula with it, in one undo step.
    #[test]
    fn backspacing_out_of_an_empty_formula_removes_it() {
        let mut editor = started_in_an_island();
        editor.in_island(Inside::Change, |editing| editing.backspace());
        assert!(editor.inside().is_none());
        assert_eq!(plain(&editor), "a");
        assert!(editor.undo());
        assert_eq!(plain(&editor), "a\u{fffc}");
    }

    #[test]
    fn entering_a_formula_from_the_right_puts_the_caret_at_its_end() {
        let mut editor = started_in_an_island();
        for c in "xy".chars() {
            editor.type_in_island(c);
        }
        assert!(editor.leave_island());
        assert_eq!(editor.primary().head, Pos::new(0, 2));
        assert!(editor.enter_island(Pos::new(0, 1), false));
        assert_eq!(editor.inside().expect("inside the formula").index, 2);
        assert!(editor.enter_island(Pos::new(0, 1), true));
        assert_eq!(editor.inside().expect("inside the formula").index, 0);
    }

    /// Moving the caret in the text leaves the formula behind: there is one
    /// caret, not one per place it could be.
    #[test]
    fn moving_in_the_text_leaves_the_formula() {
        let mut editor = started_in_an_island();
        editor.move_h(false, false);
        assert!(editor.inside().is_none());
    }
}
