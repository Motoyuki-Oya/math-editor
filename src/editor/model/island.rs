//! The bridge into an island: stepping in and out, and handing commands to
//! [`crate::structure::edit`], which does the editing inside.
//!
//! The caret is one place in one document either way; `inside` only says how
//! deep into that place it reaches. Commands that mean something different
//! inside a structure (Tab, Enter, the arrows) stay in
//! [`super::Editor`]'s own methods and call in here.

use super::history::Step;
use super::Editor;
use crate::structure::ast::{row_at, Cursor, Node, Row};
use crate::structure::edit::{Editing, Escape};
use crate::structure::text::{before_col, before_pos, Item, Pos, Sel};

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

impl Editor {
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
        self.history.cut();
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

    /// Steps into the island at `at`, with the caret where a click landed.
    pub fn enter_island_at(&mut self, at: Pos, cursor: &Cursor) -> bool {
        if !self.enter_island(at, true) {
            return false;
        }
        let to = cursor.clone();
        self.in_island(Inside::Move, move |editing| {
            editing.set_cursor(to);
            None
        })
    }

    /// Steps back out of the island, leaving the caret beside it.
    pub fn leave_island(&mut self) -> bool {
        if self.inside.take().is_none() {
            return false;
        }
        let at = self.primary().head;
        self.history.cut();
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
            Inside::Move | Inside::Extend => self.history.cut(),
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

    /// Shows a stretch of a row inside the island at `at`, which is how a match
    /// found in a structure is selected.
    pub fn select_in_island(&mut self, at: Pos, cursor: Cursor) -> bool {
        if !matches!(self.text.item_at(at), Some(Item::Math(_))) {
            return false;
        }
        self.history.cut();
        self.sels = vec![Sel::caret(at)];
        self.inside = Some(cursor);
        true
    }

    /// Replaces a stretch of a row inside the island at `at`. The replacement
    /// goes in as plain characters, so replacing never builds a structure by
    /// accident.
    pub fn replace_in_island(&mut self, at: Pos, cursor: Cursor, with: &str) -> bool {
        if !self.select_in_island(at, cursor) {
            return false;
        }
        // A structure holds one row and no column separators, so neither has
        // anything to mean in there.
        let nodes: Row = with
            .chars()
            .filter(|c| *c != '\n' && *c != '\t')
            .map(Node::Char)
            .collect();
        self.insert_row_in_island(nodes)
    }

    /// Selects the island the caret is inside as one item of the text, which is
    /// what a selection that outgrew the structure means.
    pub fn select_island(&mut self) -> bool {
        if self.inside.take().is_none() {
            return false;
        }
        let at = self.primary().head;
        self.history.cut();
        let after = self.text.clamp(Pos::new(at.line, at.col + 1));
        self.sels = vec![Sel::range(at, after)];
        true
    }

    /// Drags the selection inside a formula out to `cursor`, keeping the anchor.
    pub fn extend_in_island(&mut self, cursor: &Cursor) -> bool {
        let to = cursor.clone();
        self.in_island(Inside::Extend, move |editing| {
            editing.extend_to(&to);
            None
        })
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

    pub(super) fn type_in_island(&mut self, c: char) -> bool {
        let mut left = false;
        let done = self.in_island(Inside::Type, |editing| {
            // A space finishes a name that was typed as a command; the shortcuts
            // belong to the structure, not to the keyboard handler.
            if c == ' ' && editing.commit_command() {
                return None;
            }
            let escape = editing.insert_char(c);
            left = escape.is_some();
            escape
        });
        // A formula that was started by typing `1/` is over once the fraction
        // is written, so what comes after it is text again and is written
        // there, not inside the formula.
        if done && left {
            let mut buffer = [0u8; 4];
            self.insert_text(c.encode_utf8(&mut buffer));
        }
        done
    }

    /// Marks the formula as lasting only until the structure being typed is
    /// written, which is what a formula started by a trigger such as `1/` is
    /// for. Anything typed after it goes back into the text.
    pub fn island_lasts_one_structure(&mut self) {
        if let Some(cursor) = self.inside.as_mut() {
            cursor.fills.insert(0, 0);
        }
    }

    /// Leaves an island the caret walked out of, taking an empty one with it:
    /// backspacing out of the front of a formula that has nothing left in it
    /// removes the formula.
    pub(super) fn escape_island(&mut self, at: Pos, escape: Escape, recorded: bool) -> bool {
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

    /// Steps into the formula the caret is about to move across, if there is one.
    pub(super) fn enter_island_beside(&mut self, forward: bool) -> bool {
        let sel = self.primary();
        if !sel.is_caret() || self.sels.len() != 1 {
            return false;
        }
        let at = if forward {
            Some(sel.head)
        } else {
            before_col(sel.head)
        };
        at.is_some_and(|at| self.enter_island(at, forward))
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{editor, plain, with_items};
    use super::*;
    use crate::editor::clipboard::Clip;

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

    /// Moving right across a formula steps into it rather than over it, so the
    /// same key does the same thing all the way through.
    #[test]
    fn moving_across_a_formula_steps_inside_it() {
        let mut editor = started_in_an_island();
        editor.type_in_island('x');
        assert!(editor.leave_island());
        editor.set_caret(Pos::new(0, 1));
        editor.move_h(true, false);
        assert_eq!(editor.inside().expect("inside the formula").index, 0);
        editor.move_h(true, false);
        assert_eq!(editor.inside().expect("inside the formula").index, 1);
        // One more step takes the caret out the far side.
        editor.move_h(true, false);
        assert!(editor.inside().is_none());
        assert_eq!(editor.primary().head, Pos::new(0, 2));
    }

    /// Typing goes through one command wherever the caret is.
    #[test]
    fn typing_reaches_whichever_place_the_caret_is_in() {
        let mut editor = started_in_an_island();
        editor.insert_text("1");
        editor.insert_text("/");
        editor.insert_text("2");
        assert!(matches!(island(&editor).as_slice(), [Node::Stack { .. }]));
        editor.leave_island();
        editor.insert_text("b");
        assert_eq!(plain(&editor), "a\u{fffc}b");
    }

    /// A paste inside a formula is text, not typing: `a/b` stays three
    /// characters instead of becoming a fraction.
    #[test]
    fn a_paste_inside_a_formula_stays_plain() {
        let mut editor = started_in_an_island();
        editor.insert_text("a/b");
        assert_eq!(
            island(&editor),
            vec![Node::Char('a'), Node::Char('/'), Node::Char('b')]
        );
    }

    /// Tab is the column separator in the text and the next slot in a formula.
    #[test]
    fn tab_steps_to_the_next_slot_inside_a_formula() {
        let mut editor = started_in_an_island();
        editor.insert_text("1");
        editor.insert_text("/");
        // In the lower row of the fraction; Tab leaves it for the row that holds
        // the fraction rather than putting a separator in.
        editor.tab(false);
        assert!(island(&editor)
            .iter()
            .all(|node| !matches!(node, Node::Char('\t'))));
        assert_eq!(
            editor.inside().expect("inside the formula").path,
            Vec::new()
        );
    }

    /// The lower row of a fraction takes one run and hands the caret back, so
    /// what is typed next lands beside the fraction, not underneath it.
    #[test]
    fn typing_on_past_a_fraction_leaves_it_behind() {
        let mut editor = started_in_an_island();
        for c in "1/2 + 3".chars() {
            editor.type_in_island(c);
        }
        assert_eq!(
            editor.inside().expect("inside the formula").path,
            Vec::new()
        );
        let row = island(&editor);
        assert!(matches!(row.first(), Some(Node::Stack { .. })));
        let after: String = row[1..]
            .iter()
            .filter_map(|node| match node {
                Node::Char(c) => Some(*c),
                _ => None,
            })
            .collect();
        assert_eq!(after, " + 3");
    }

    /// A formula that a trigger put there holds the structure that called it
    /// up and nothing more: what is typed after it is text again.
    #[test]
    fn a_formula_a_trigger_made_ends_with_its_structure() {
        let mut editor = started_in_an_island();
        editor.island_lasts_one_structure();
        for c in "1/2 + 3".chars() {
            editor.insert_text(&c.to_string());
        }
        assert!(editor.inside().is_none());
        editor.set_caret(Pos::new(0, 1));
        assert_eq!(island(&editor).len(), 1);
        assert!(matches!(island(&editor).first(), Some(Node::Stack { .. })));
        assert_eq!(plain(&editor), "a\u{fffc} + 3");
    }

    /// Enter and Escape end the formula instead of splitting the line.
    #[test]
    fn enter_and_escape_leave_a_formula() {
        let mut editor = started_in_an_island();
        editor.insert_text("x");
        editor.split_line();
        assert!(editor.inside().is_none());
        assert_eq!(plain(&editor), "a\u{fffc}");
        editor.enter_island(Pos::new(0, 1), true);
        editor.escape();
        assert!(editor.inside().is_none());
    }

    /// A match found inside a structure is replaced where it was found, and the
    /// replacement stays plain characters.
    #[test]
    fn a_replacement_reaches_inside_a_formula() {
        let mut editor = started_in_an_island();
        for c in "ab".chars() {
            editor.type_in_island(c);
        }
        editor.leave_island();
        let at = Pos::new(0, 1);
        let found = Cursor {
            path: Vec::new(),
            anchor: 0,
            index: 1,
            fills: Vec::new(),
        };
        assert!(editor.replace_in_island(at, found, "x/y"));
        editor.set_caret(at);
        assert_eq!(
            island(&editor),
            vec![
                Node::Char('x'),
                Node::Char('/'),
                Node::Char('y'),
                Node::Char('b'),
            ]
        );
        assert!(editor.undo());
        assert_eq!(island(&editor), vec![Node::Char('a'), Node::Char('b')]);
    }

    /// A structure copied out of the text goes back in as a structure, not as
    /// the characters it reads as.
    #[test]
    fn a_copied_structure_pastes_back_as_itself() {
        let fraction = vec![Node::Stack {
            above: vec![Node::Char('a')],
            below: vec![Node::Char('b')],
            between: crate::structure::ast::Between::Rule,
        }];
        let mut editor = with_items(vec![vec![Item::Math(fraction.clone())]]);
        editor.set_caret(Pos::new(0, 1));
        let clip = Clip::Text(vec![vec![Item::Math(fraction.clone())]]);
        editor.insert_clip(&clip);
        assert_eq!(
            editor.text().line(0),
            &[Item::Math(fraction.clone()), Item::Math(fraction.clone()),]
        );
    }

    /// A structure is one row, so a pasted line break is dropped rather than
    /// put in as a character the file could not hold.
    #[test]
    fn pasting_lines_inside_a_structure_keeps_one_row() {
        let mut editor = started_in_an_island();
        editor.insert_text("ab\ncd");
        assert_eq!(
            island(&editor),
            vec![
                Node::Char('a'),
                Node::Char('b'),
                Node::Char('c'),
                Node::Char('d')
            ]
        );
    }

    /// Pasting into a structure puts the copied pieces in that row, so a
    /// fraction pasted into a denominator is a fraction there too.
    #[test]
    fn a_copied_piece_pastes_inside_a_structure() {
        let mut editor = started_in_an_island();
        let piece: Row = vec![Node::Char('a'), Node::Char('b')];
        editor.insert_clip(&Clip::Row(piece));
        assert_eq!(island(&editor), vec![Node::Char('a'), Node::Char('b')]);
    }

    /// Inside a structure there is one caret, so Ctrl+D does nothing rather
    /// than selecting a word of the text the caret is standing on.
    #[test]
    fn ctrl_d_does_nothing_inside_a_formula() {
        let mut editor = started_in_an_island();
        editor.insert_text("a");
        editor.insert_text("b");
        assert!(!editor.add_next_occurrence());
        assert_eq!(editor.sels().len(), 1);
        assert!(editor.inside().is_some());
        // Typing still goes into the structure.
        editor.insert_text("c");
        assert_eq!(
            island(&editor),
            vec![Node::Char('a'), Node::Char('b'), Node::Char('c')]
        );
    }

    /// Turning typed text into a structure is one step of the history: undo
    /// gives back the characters, not the formula they went into.
    #[test]
    fn a_shortcut_that_builds_a_structure_is_one_undo() {
        let mut editor = editor("x1");
        editor.set_caret(Pos::new(0, 2));
        editor.one_step(|editor| {
            editor.replace_range(Pos::new(0, 0), Pos::new(0, 2), "");
            editor.insert_island();
            for c in "x1/".chars() {
                editor.insert_text(&c.to_string());
            }
        });
        assert!(matches!(island(&editor).as_slice(), [Node::Stack { .. }]));
        assert!(editor.undo());
        assert_eq!(plain(&editor), "x1");
        // Nothing in between: the half-built formula was never a step.
        assert!(!editor.undo());
        assert!(editor.redo());
        assert!(matches!(island(&editor).as_slice(), [Node::Stack { .. }]));
    }

    /// A selection inside a formula can be dragged, which is the same selection
    /// the keyboard makes.
    #[test]
    fn dragging_inside_a_formula_selects_the_same_way() {
        let mut editor = started_in_an_island();
        for c in "abc".chars() {
            editor.type_in_island(c);
        }
        editor.in_island(Inside::Move, |editing| {
            editing.move_to_start();
            None
        });
        assert!(editor.extend_in_island(&Cursor {
            path: Vec::new(),
            anchor: 0,
            index: 2,
            fills: Vec::new(),
        }));
        assert_eq!(
            editor.island_selection(),
            Some(vec![Node::Char('a'), Node::Char('b')])
        );
        // Dragging out of the formula takes it whole.
        assert!(editor.select_island());
        assert_eq!(editor.primary(), Sel::range(Pos::new(0, 1), Pos::new(0, 2)));
    }
}
