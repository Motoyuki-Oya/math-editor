//! Editing a document: the selections, the commands they apply, and the undo
//! history.
//!
//! Every command applies to all of the selections as a single step, the way
//! multiple cursors are expected to behave. The document itself is
//! [`crate::structure::text`], which knows nothing about the notation or the
//! screen.

mod history;
mod island;

use crate::structure::edit::Escape;
use history::{History, Step};
pub use island::Inside;

use super::clipboard::Clip;
use crate::structure::ast::{Cursor, Node, Row};
use crate::structure::text::{items_of, Item, Pos, Sel, Text};

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

/// What a command did, which is all the caller needs in order to react: there
/// is no mode to ask about, only whether anything moved or changed.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Did {
    /// The key means nothing here, so it belongs to whoever else wants it.
    Nothing,
    /// The caret or the selection moved.
    Moved,
    /// The document changed.
    Changed,
}

impl Did {
    fn moved(happened: bool) -> Did {
        if happened {
            Did::Moved
        } else {
            Did::Nothing
        }
    }
}

pub struct Editor {
    text: Text,
    sels: Vec<Sel>,
    /// Where the caret is inside the island it stands on, when it is in one.
    /// The caret is one place in one document either way: this only says how
    /// deep into that place it reaches.
    inside: Option<Cursor>,
    history: History,
}

impl Default for Editor {
    fn default() -> Self {
        Self {
            text: Text::default(),
            sels: vec![Sel::caret(Pos::default())],
            inside: None,
            history: History::default(),
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
        self.history.clear();
    }

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

    /// Puts text in at the caret, wherever the caret is. A single character is
    /// typed, so the shortcuts inside a structure still run; anything longer is
    /// a paste and goes in as it is.
    pub fn insert_text(&mut self, text: &str) -> Did {
        if self.inside.is_some() {
            let mut chars = text.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => self.type_in_island(c),
                // Characters stay characters: a paste never re-runs the
                // shortcuts that typing them would. A structure holds one row,
                // so a line break has nothing to mean inside it.
                _ => self.insert_row_in_island(
                    text.chars()
                        .filter(|c| *c != '\n')
                        .map(Node::Char)
                        .collect(),
                ),
            };
            return Did::Changed;
        }
        self.insert(items_of(text));
        Did::Changed
    }

    /// Puts a piece that was copied out of a document back in, with the shape it
    /// had. Text from anywhere else arrives through [`Self::insert_text`] as the
    /// characters it is.
    pub fn insert_clip(&mut self, clip: &Clip) -> Did {
        if self.inside.is_some() {
            self.insert_row_in_island(clip.row());
        } else {
            self.insert(clip.items());
        }
        Did::Changed
    }

    pub fn insert_math(&mut self, row: Row) {
        self.insert(vec![vec![Item::Math(row)]]);
    }

    /// Tab: a column separator in the text, and a step to the next slot inside
    /// a structure, which is what Tab means in every formula editor.
    pub fn tab(&mut self, back: bool) -> Did {
        if self.inside.is_some() {
            self.in_island(Inside::Move, |editing| {
                if back {
                    editing.move_left()
                } else {
                    editing.move_right()
                }
            });
            return Did::Moved;
        }
        self.insert(vec![vec![Item::Tab]]);
        Did::Changed
    }

    /// Enter: a new line in the text, and the end of the formula inside one.
    pub fn split_line(&mut self) -> Did {
        if self.leave_island() {
            return Did::Moved;
        }
        self.insert(vec![Vec::new(), Vec::new()]);
        Did::Changed
    }

    /// Escape: leaves the formula, or drops the extra cursors.
    pub fn escape(&mut self) -> Did {
        Did::moved(self.leave_island() || self.collapse_sels())
    }

    pub fn backspace(&mut self) -> Did {
        if self.inside.is_some() {
            self.in_island(Inside::Change, |editing| editing.backspace());
            return Did::Changed;
        }
        self.backspace_in_text();
        Did::Changed
    }

    fn backspace_in_text(&mut self) {
        self.edit_each(Step::Other, |text, sel| {
            if sel.is_caret() {
                (before(text, sel.head), sel.head, Vec::new())
            } else {
                (sel.start(), sel.end(), Vec::new())
            }
        });
    }

    pub fn delete_forward(&mut self) -> Did {
        if self.inside.is_some() {
            self.in_island(Inside::Change, |editing| {
                editing.delete_forward();
                None
            });
            return Did::Changed;
        }
        self.edit_each(Step::Other, |text, sel| {
            if sel.is_caret() {
                (sel.head, after(text, sel.head), Vec::new())
            } else {
                (sel.start(), sel.end(), Vec::new())
            }
        });
        Did::Changed
    }

    /// Grows the grid the caret is in by a row, which only means anything inside
    /// a structure.
    pub fn grow_matrix(&mut self) -> Did {
        if self.inside.is_none() {
            return Did::Nothing;
        }
        self.in_island(Inside::Change, |editing| {
            editing.grow_matrix(true);
            None
        });
        Did::Changed
    }

    /// Replaces one range, used by search and replace.
    pub fn replace_range(&mut self, from: Pos, to: Pos, with: &str) {
        self.replace_range_with(from, to, items_of(with));
    }

    /// Replaces a range with items, for a replacement that puts in more than
    /// characters: a column separator is an item of its own.
    pub fn replace_range_with(&mut self, from: Pos, to: Pos, with: Vec<Vec<Item>>) {
        self.record(Step::Other);
        self.inside = None;
        let at = self.text.remove(from, to);
        let end = self.text.insert(at, with);
        self.sels = vec![Sel::caret(end)];
    }

    /// Left and right, one place at a time. A formula is a place the caret goes
    /// into rather than over, and inside one the places are the structure's own.
    pub fn move_h(&mut self, forward: bool, extend: bool) -> Did {
        if self.inside.is_some() {
            let kind = if extend { Inside::Extend } else { Inside::Move };
            self.in_island(kind, |editing| {
                if extend {
                    editing.extend(forward)
                } else if forward {
                    editing.move_right()
                } else {
                    editing.move_left()
                }
            });
            return Did::Moved;
        }
        if !extend && self.enter_island_beside(forward) {
            return Did::Moved;
        }
        self.map_sels(extend, |text, head| {
            if forward {
                after(text, head)
            } else {
                before(text, head)
            }
        });
        Did::Moved
    }

    /// Up and down: between the lines of the text, and between the slots of the
    /// structure the caret is in. Leaving the top or the bottom of a formula
    /// puts the caret back in the text.
    pub fn move_v(&mut self, down: bool, extend: bool) -> Did {
        if self.inside.is_some() {
            self.in_island(Inside::Move, |editing| {
                let moved = if down {
                    editing.move_down()
                } else {
                    editing.move_up()
                };
                (!moved).then_some(Escape::Done)
            });
            return Did::Moved;
        }
        self.map_sels(extend, |text, head| {
            let line = if down {
                head.line + 1
            } else {
                head.line.checked_sub(1).unwrap_or(head.line)
            };
            text.clamp(Pos::new(line.min(text.line_count() - 1), head.col))
        });
        Did::Moved
    }

    pub fn move_line_edge(&mut self, end: bool, extend: bool) -> Did {
        if self.inside.is_some() {
            self.in_island(Inside::Move, |editing| {
                if end {
                    editing.move_end();
                } else {
                    editing.move_home();
                }
                None
            });
            return Did::Moved;
        }
        self.map_sels(extend, |text, head| {
            Pos::new(head.line, if end { text.line_len(head.line) } else { 0 })
        });
        Did::Moved
    }

    pub fn move_document_edge(&mut self, end: bool, extend: bool) -> Did {
        self.leave_island();
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
        Did::Moved
    }

    fn map_sels(&mut self, extend: bool, step: impl Fn(&Text, Pos) -> Pos) {
        self.history.cut();
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
        self.history.cut();
        self.inside = None;
        self.sels = vec![Sel::caret(self.text.clamp(at))];
    }

    pub fn extend_to(&mut self, at: Pos) {
        self.history.cut();
        self.inside = None;
        let at = self.text.clamp(at);
        if let Some(sel) = self.sels.last_mut() {
            sel.head = at;
        }
        self.merge_sels();
    }

    pub fn add_caret(&mut self, at: Pos) {
        self.history.cut();
        self.inside = None;
        self.sels.push(Sel::caret(self.text.clamp(at)));
        self.merge_sels();
    }

    /// Selects everything there is to select where the caret is: the row of the
    /// structure it is in, or the whole document.
    pub fn select_all(&mut self) -> Did {
        if self.inside.is_some() {
            self.in_island(Inside::Extend, |editing| {
                editing.select_row();
                None
            });
            return Did::Moved;
        }
        self.history.cut();
        self.sels = vec![Sel::range(Pos::default(), self.text.end())];
        Did::Moved
    }

    pub fn set_sels(&mut self, sels: Vec<Sel>) {
        self.history.cut();
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
        // A structure holds one caret, so there is nothing to add there.
        if self.inside.is_some() {
            return false;
        }
        self.history.cut();
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
pub(crate) mod tests {
    use super::*;
    /// Nothing here goes through the notation: the model is only ever handed
    /// items, so these tests cannot start depending on the file format.
    pub(crate) fn editor(source: &str) -> Editor {
        with_items(items_of(source))
    }

    pub(crate) fn with_items(lines: Vec<Vec<Item>>) -> Editor {
        let mut editor = Editor::default();
        editor.load(Text::from_lines(lines));
        editor
    }

    /// The text of the document, with each island standing in as one character.
    pub(crate) fn plain(editor: &Editor) -> String {
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
        editor.tab(false);
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
}
