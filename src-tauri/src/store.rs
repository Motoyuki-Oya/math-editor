//! 文書の本体。webview には見えている窓の行だけを渡し、編集・元に戻す・保存は
//! ここで行う。frontend は「行範囲の置き換え」を送ってくるだけで、文書の真実は
//! 常にこちら側にある。
//!
//! 開いたファイルの中身はひと続きの文字列のまま持ち、行はその切り出しで表す。
//! 編集された行だけが独自の文字列を持つので、メモリはファイルと編集量の分で済む。

use std::io::{BufWriter, Write};

/// 1 行。開いたときのままの行は元の文字列の範囲、編集で入った行は所有する文字列。
#[derive(Clone, Debug)]
enum Line {
    Slice(usize, usize),
    Owned(String),
}

/// 履歴の 1 かたまりに入る 1 つの置き換えの逆: `from` から `inserted` 行を
/// 取り除き、`removed` を戻すと元に戻る。
#[derive(Debug)]
struct Edit {
    from: usize,
    removed: Vec<String>,
    inserted: usize,
}

/// 履歴の 1 ステップ。同じ `group` の置き換えが続く間は 1 つにつながるので、
/// 「すべて置換」も入力の 1 操作も、1 回の元に戻すで全部戻る。
#[derive(Debug)]
struct Step {
    group: u64,
    edits: Vec<Edit>,
    /// 編集前後のキャレットなどの控え。frontend が渡す不透明な文字列で、
    /// こちらは中身を解釈しない。
    before: String,
    after: String,
}

/// 履歴が持つステップ数の上限。それより古いものは忘れる。
const HISTORY_LIMIT: usize = 1000;

pub struct Document {
    original: String,
    lines: Vec<Line>,
    undo: Vec<Step>,
    redo: Vec<Step>,
}

/// 元に戻す・やり直すの結果: 復元すべき控えと、行が変わった範囲の始まり。
/// frontend は `touched_from` から先の手元の行を捨てて取り寄せ直す。
pub struct Restored {
    pub state: String,
    pub touched_from: usize,
    pub line_count: usize,
}

impl Document {
    pub fn open(source: String) -> Document {
        let mut lines = Vec::with_capacity(source.len() / 32 + 1);
        let mut start = 0;
        for at in memchr::memchr_iter(b'\n', source.as_bytes()) {
            lines.push(Line::Slice(start, at));
            start = at + 1;
        }
        lines.push(Line::Slice(start, source.len()));
        Document {
            original: source,
            lines,
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    pub fn empty() -> Document {
        Document::open(String::new())
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn bytes(&self) -> usize {
        self.original.len()
    }

    fn line(&self, index: usize) -> &str {
        match &self.lines[index] {
            Line::Slice(start, end) => &self.original[*start..*end],
            Line::Owned(line) => line,
        }
    }

    pub fn read(&self, from: usize, count: usize) -> Vec<String> {
        let to = from.saturating_add(count).min(self.lines.len());
        (from.min(to)..to)
            .map(|i| self.line(i).to_string())
            .collect()
    }

    /// `from..to` の行を `lines` に置き換え、逆操作を履歴に書く。
    /// 直前のステップと同じ `group` なら 1 ステップにつながる。
    pub fn replace(
        &mut self,
        from: usize,
        to: usize,
        lines: Vec<String>,
        group: u64,
        before: &str,
        after: &str,
    ) -> Result<usize, String> {
        if from > to || to > self.lines.len() {
            return Err("置き換えの範囲が文書の外です".to_string());
        }
        let edit = self.splice(from, to, lines);
        self.redo.clear();
        match self.undo.last_mut() {
            Some(step) if step.group == group => {
                step.edits.push(edit);
                step.after = after.to_string();
            }
            _ => {
                self.undo.push(Step {
                    group,
                    edits: vec![edit],
                    before: before.to_string(),
                    after: after.to_string(),
                });
                if self.undo.len() > HISTORY_LIMIT {
                    self.undo.remove(0);
                }
            }
        }
        Ok(self.lines.len())
    }

    /// 置き換えの本体。履歴には触らず、逆操作を返す。
    fn splice(&mut self, from: usize, to: usize, lines: Vec<String>) -> Edit {
        let inserted = lines.len();
        let removed: Vec<String> = self
            .lines
            .splice(from..to, lines.into_iter().map(Line::Owned))
            .map(|line| match line {
                Line::Slice(start, end) => self.original[start..end].to_string(),
                Line::Owned(line) => line,
            })
            .collect();
        Edit {
            from,
            removed,
            inserted,
        }
    }

    pub fn undo(&mut self) -> Option<Restored> {
        let step = self.undo.pop()?;
        let (reverted, touched_from) = self.revert(&step);
        let state = step.before.clone();
        self.redo.push(reverted);
        Some(Restored {
            state,
            touched_from,
            line_count: self.lines.len(),
        })
    }

    pub fn redo(&mut self) -> Option<Restored> {
        let step = self.redo.pop()?;
        let (reverted, touched_from) = self.revert(&step);
        let state = step.after.clone();
        self.undo.push(reverted);
        Some(Restored {
            state,
            touched_from,
            line_count: self.lines.len(),
        })
    }

    /// ステップの置き換えを新しい順に巻き戻し、巻き戻し自体を巻き戻すステップを返す。
    fn revert(&mut self, step: &Step) -> (Step, usize) {
        let mut inverse = Vec::with_capacity(step.edits.len());
        let mut touched_from = usize::MAX;
        for edit in step.edits.iter().rev() {
            touched_from = touched_from.min(edit.from);
            inverse.push(self.splice(edit.from, edit.from + edit.inserted, edit.removed.clone()));
        }
        // 巻き戻しの逆は元のステップと同じ向き。適用した順の逆で持つ。
        (
            Step {
                group: step.group,
                edits: inverse.into_iter().rev().collect(),
                before: step.before.clone(),
                after: step.after.clone(),
            },
            if touched_from == usize::MAX {
                0
            } else {
                touched_from
            },
        )
    }

    /// `from..=to` の行のうち `needle` を含むもの。
    pub fn lines_containing(&self, from: usize, to: usize, needle: char) -> Vec<usize> {
        let to = to.min(self.lines.len().saturating_sub(1));
        (from.min(to)..=to)
            .filter(|&i| self.line(i).contains(needle))
            .collect()
    }

    /// 選択された範囲をひとつなぎのテキストにする。`first` / `last` は端の行の
    /// 切り出し（`None` なら行を丸ごと）。`overrides` の行は差し替えて使う。
    pub fn assemble(
        &self,
        from: usize,
        first: Option<String>,
        to: usize,
        last: Option<String>,
        overrides: &std::collections::HashMap<usize, String>,
    ) -> Result<String, String> {
        if from > to || to >= self.lines.len() {
            return Err("コピーの範囲が文書の外です".to_string());
        }
        let piece = |i: usize| -> &str {
            match overrides.get(&i) {
                Some(text) => text,
                None => self.line(i),
            }
        };
        let mut out = String::new();
        match &first {
            Some(text) => out.push_str(text),
            None => out.push_str(piece(from)),
        }
        for i in from + 1..to {
            out.push('\n');
            out.push_str(piece(i));
        }
        if to > from {
            out.push('\n');
            match &last {
                Some(text) => out.push_str(text),
                None => out.push_str(piece(to)),
            }
        }
        Ok(out)
    }

    /// 文書の行を書き手へ流す。全文を 1 つの文字列に集めない。
    pub fn write_to<W: Write>(&self, out: &mut W) -> std::io::Result<()> {
        for (index, _) in self.lines.iter().enumerate() {
            if index > 0 {
                out.write_all(b"\n")?;
            }
            out.write_all(self.line(index).as_bytes())?;
        }
        out.flush()
    }

    /// 文書をそのままディスクへ流す。
    pub fn save(&self, path: &str) -> Result<(), String> {
        let file = std::fs::File::create(path)
            .map_err(|e| format!("{path} を保存できませんでした: {e}"))?;
        self.write_to(&mut BufWriter::new(file))
            .map_err(|e| format!("{path} を保存できませんでした: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(lines: &[&str]) -> Document {
        Document::open(lines.join("\n"))
    }

    #[test]
    fn opening_splits_lines_without_copying_them() {
        let doc = doc(&["ab", "", "cd"]);
        assert_eq!(doc.line_count(), 3);
        assert_eq!(doc.read(0, 10), vec!["ab", "", "cd"]);
        assert_eq!(doc.read(2, 1), vec!["cd"]);
    }

    #[test]
    fn replacing_lines_and_undoing_restores_the_document() {
        let mut doc = doc(&["a", "b", "c"]);
        doc.replace(1, 2, vec!["X".into(), "Y".into()], 1, "before", "after")
            .unwrap();
        assert_eq!(doc.read(0, 10), vec!["a", "X", "Y", "c"]);
        let undone = doc.undo().unwrap();
        assert_eq!(undone.state, "before");
        assert_eq!(undone.touched_from, 1);
        assert_eq!(doc.read(0, 10), vec!["a", "b", "c"]);
        let redone = doc.redo().unwrap();
        assert_eq!(redone.state, "after");
        assert_eq!(doc.read(0, 10), vec!["a", "X", "Y", "c"]);
    }

    #[test]
    fn steps_in_the_same_group_undo_together() {
        let mut doc = doc(&["a", "b", "c"]);
        // 「すべて置換」のように、複数の置き換えが 1 つのグループで届く。
        doc.replace(2, 3, vec!["C".into()], 7, "start", "mid")
            .unwrap();
        doc.replace(0, 1, vec!["A".into()], 7, "ignored", "end")
            .unwrap();
        assert_eq!(doc.read(0, 10), vec!["A", "b", "C"]);
        let undone = doc.undo().unwrap();
        assert_eq!(doc.read(0, 10), vec!["a", "b", "c"]);
        assert_eq!(undone.state, "start");
        assert_eq!(undone.touched_from, 0);
        assert!(doc.undo().is_none());
        let redone = doc.redo().unwrap();
        assert_eq!(doc.read(0, 10), vec!["A", "b", "C"]);
        assert_eq!(redone.state, "end");
    }

    #[test]
    fn different_groups_undo_one_at_a_time() {
        let mut doc = doc(&["a"]);
        doc.replace(0, 1, vec!["b".into()], 1, "s1", "e1").unwrap();
        doc.replace(0, 1, vec!["c".into()], 2, "s2", "e2").unwrap();
        doc.undo().unwrap();
        assert_eq!(doc.read(0, 10), vec!["b"]);
        doc.undo().unwrap();
        assert_eq!(doc.read(0, 10), vec!["a"]);
    }

    #[test]
    fn a_new_edit_clears_the_redo_branch() {
        let mut doc = doc(&["a"]);
        doc.replace(0, 1, vec!["b".into()], 1, "", "").unwrap();
        doc.undo().unwrap();
        doc.replace(0, 1, vec!["c".into()], 2, "", "").unwrap();
        assert!(doc.redo().is_none());
        assert_eq!(doc.read(0, 10), vec!["c"]);
    }

    #[test]
    fn out_of_range_replacements_are_rejected() {
        let mut doc = doc(&["a"]);
        assert!(doc.replace(0, 2, vec![], 1, "", "").is_err());
        assert!(doc.replace(1, 0, vec![], 1, "", "").is_err());
    }

    #[test]
    fn assembling_a_range_uses_edges_and_overrides() {
        let doc = doc(&["aa", "bb", "cc", "dd"]);
        let overrides = std::collections::HashMap::from([(2usize, "CC".to_string())]);
        assert_eq!(
            doc.assemble(0, Some("a".into()), 3, Some("d".into()), &overrides)
                .unwrap(),
            "a\nbb\nCC\nd"
        );
        assert_eq!(
            doc.assemble(1, None, 1, None, &Default::default()).unwrap(),
            "bb"
        );
        assert!(doc.assemble(0, None, 4, None, &Default::default()).is_err());
    }

    #[test]
    fn finding_lines_that_contain_a_character() {
        let doc = doc(&["aa", "a$b", "cc", "$"]);
        assert_eq!(doc.lines_containing(0, 3, '$'), vec![1, 3]);
        assert_eq!(doc.lines_containing(0, 99, '$'), vec![1, 3]);
    }

    /// 規模の実測: `cargo test -p planetext --release -- --ignored --nocapture`。
    /// C:\workspace\test-800mb.txt がある環境でだけ動く。
    #[test]
    #[ignore]
    fn scale_check_opening_a_huge_file() {
        let path = r"C:\workspace\test-800mb.txt";
        let Ok(source) = std::fs::read_to_string(path) else {
            return;
        };
        let bytes = source.len();
        let start = std::time::Instant::now();
        let doc = Document::open(source);
        println!(
            "open (scan {} MB, {} lines): {:?}",
            bytes / 1_000_000,
            doc.line_count(),
            start.elapsed()
        );
        let start = std::time::Instant::now();
        let _ = doc.read(doc.line_count() / 2, 100);
        println!("read 100 lines: {:?}", start.elapsed());
    }

    #[test]
    fn saving_writes_the_lines_back() {
        let mut doc = doc(&["a", "b"]);
        doc.replace(1, 2, vec!["B".into()], 1, "", "").unwrap();
        let path = std::env::temp_dir().join("planetext-store-test.txt");
        let path = path.to_string_lossy().into_owned();
        doc.save(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a\nB");
        std::fs::remove_file(&path).ok();
    }
}
