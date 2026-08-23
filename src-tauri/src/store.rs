//! 文書の本体。webview には見えている窓の行だけを渡し、編集・元に戻す・保存は
//! ここで行う。frontend は「行範囲の置き換え」を送ってくるだけで、文書の真実は
//! 常にこちら側にある。
//!
//! 本文はメモリに置かない。開くときに改行を 1 度だけ数え、行の場所は
//! [`STRIDE`] 行ごとの行頭のバイト位置（間引きの索引）だけを控える。行の
//! 中身が要るときは、最寄りの索引へ seek してそこから読み流す。編集は
//! ピース列で表し、ディスクにそのまま残っている範囲と、編集で入った行だけ
//! を持つ。メモリは索引と編集量の分で済む。

use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// 何行ごとに行頭のバイト位置を控えるか。1600 万行でも索引は数万個で済み、
/// どの行へも高々この行数だけ読み流せば届く。
const STRIDE: usize = 1024;

/// 履歴が持つステップ数の上限。それより古いものは忘れる。
const HISTORY_LIMIT: usize = 1000;

/// ファイルを読むときのひとかたまり。
const CHUNK: usize = 1 << 20;

/// 開いたファイル。行の中身はここから seek で読む。
struct Source {
    path: PathBuf,
    file: File,
    /// STRIDE 行ごとの行頭のバイト位置。先頭は必ず 0。
    marks: Vec<u64>,
    /// ディスク上の行数。
    lines: usize,
    bytes: u64,
    /// 開いたときの姿。外から書き換えられると seek 読みが壊れるので、
    /// 変わっていたら読む前に断る。
    modified: Option<SystemTime>,
}

impl Source {
    /// ファイルを 1 度だけ読み流し、改行を数えて間引きの索引を作る。
    fn open(path: &Path) -> Result<Source, String> {
        let file =
            File::open(path).map_err(|e| format!("{} を開けませんでした: {e}", path.display()))?;
        let modified = file.metadata().ok().and_then(|meta| meta.modified().ok());
        let mut reader = BufReader::with_capacity(CHUNK, &file);
        let mut marks = vec![0u64];
        let mut line = 0usize;
        let mut offset = 0u64;
        let mut carry: Vec<u8> = Vec::new();
        loop {
            let chunk = reader
                .fill_buf()
                .map_err(|e| format!("{} を読めませんでした: {e}", path.display()))?;
            if chunk.is_empty() {
                if !carry.is_empty() {
                    return Err(format!("{} は UTF-8 ではありません", path.display()));
                }
                break;
            }
            for at in memchr::memchr_iter(b'\n', chunk) {
                line += 1;
                if line.is_multiple_of(STRIDE) {
                    marks.push(offset + at as u64 + 1);
                }
            }
            if !valid_utf8(&mut carry, chunk) {
                return Err(format!("{} は UTF-8 ではありません", path.display()));
            }
            let len = chunk.len();
            offset += len as u64;
            reader.consume(len);
        }
        drop(reader);
        Ok(Source {
            path: path.to_path_buf(),
            file,
            marks,
            lines: line + 1,
            bytes: offset,
            modified,
        })
    }

    /// 開いてから外で書き換えられていないか。壊れた seek 読みを返すより断る。
    fn check(&self) -> Result<(), String> {
        let meta = std::fs::metadata(&self.path)
            .map_err(|e| format!("{} を読めませんでした: {e}", self.path.display()))?;
        let same = meta.len() == self.bytes && meta.modified().ok() == self.modified;
        if same {
            Ok(())
        } else {
            Err(format!(
                "{} が別の場所で変更されました。開き直してください",
                self.path.display()
            ))
        }
    }

    /// ディスク上の行 `from` から `count` 行に `f` を呼ぶ。`f` が `false` を
    /// 返したら打ち切る。最寄りの索引へ seek し、そこから読み流す。
    fn each_line(
        &mut self,
        from: usize,
        count: usize,
        f: &mut dyn FnMut(usize, &str) -> bool,
    ) -> Result<(), String> {
        if count == 0 || from >= self.lines {
            return Ok(());
        }
        self.check()?;
        let broken = |e| format!("{} を読めませんでした: {e}", self.path.display());
        let mark = self.marks[from / STRIDE];
        self.file.seek(SeekFrom::Start(mark)).map_err(broken)?;
        let mut reader = BufReader::with_capacity(CHUNK, &self.file);
        let mut buffer = Vec::new();
        // 索引からその行までの読み飛ばし。高々 STRIDE 行。
        for _ in 0..from % STRIDE {
            buffer.clear();
            reader.read_until(b'\n', &mut buffer).map_err(broken)?;
        }
        let to = (from + count).min(self.lines);
        for line in from..to {
            buffer.clear();
            reader.read_until(b'\n', &mut buffer).map_err(broken)?;
            if buffer.last() == Some(&b'\n') {
                buffer.pop();
            }
            if !f(line, &String::from_utf8_lossy(&buffer)) {
                return Ok(());
            }
        }
        Ok(())
    }
}

/// チャンクの並びが UTF-8 かどうか。文字の途中でチャンクが切れることがある
/// ので、切れ端は `carry` に持ち越して次のチャンクの頭と合わせて見る。
fn valid_utf8(carry: &mut Vec<u8>, mut chunk: &[u8]) -> bool {
    while !carry.is_empty() && !chunk.is_empty() {
        carry.push(chunk[0]);
        chunk = &chunk[1..];
        match std::str::from_utf8(carry) {
            Ok(_) => carry.clear(),
            Err(e) if e.error_len().is_some() || carry.len() >= 4 => return false,
            Err(_) => {}
        }
    }
    match std::str::from_utf8(chunk) {
        Ok(_) => true,
        Err(e) if e.error_len().is_some() => false,
        Err(e) => {
            *carry = chunk[e.valid_up_to()..].to_vec();
            true
        }
    }
}

/// 文書のひと続き: ディスクにそのまま残っている行の範囲か、編集で入った行。
#[derive(Debug)]
enum Piece {
    Disk { from: usize, lines: usize },
    Fresh(Vec<String>),
}

impl Piece {
    fn len(&self) -> usize {
        match self {
            Piece::Disk { lines, .. } => *lines,
            Piece::Fresh(lines) => lines.len(),
        }
    }
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

pub struct Document {
    source: Option<Source>,
    pieces: Vec<Piece>,
    /// すべてのピースの行数の合計。
    count: usize,
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

/// 検索走査の 1 件。`notation` の行は一致ではなく「frontend が見るべき行」。
#[derive(serde::Serialize)]
pub struct ScanHit {
    pub line: usize,
    pub notation: bool,
    /// 行内の一致の文字位置。`notation` の行では意味を持たない。
    pub start: usize,
    pub end: usize,
}

impl Document {
    pub fn open(path: &str) -> Result<Document, String> {
        let source = Source::open(Path::new(path))?;
        let count = source.lines;
        Ok(Document {
            pieces: vec![Piece::Disk {
                from: 0,
                lines: count,
            }],
            count,
            source: Some(source),
            undo: Vec::new(),
            redo: Vec::new(),
        })
    }

    pub fn empty() -> Document {
        Document {
            source: None,
            pieces: vec![Piece::Fresh(vec![String::new()])],
            count: 1,
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    pub fn line_count(&self) -> usize {
        self.count
    }

    pub fn bytes(&self) -> usize {
        self.source
            .as_ref()
            .map_or(0, |source| source.bytes as usize)
    }

    /// 文書の行 `from..from+count` に `f` を呼ぶ。ディスクの範囲は seek して
    /// 読み流し、編集で入った行はそのまま渡す。`f` が `false` で打ち切り。
    fn each_line(
        &mut self,
        from: usize,
        count: usize,
        f: &mut dyn FnMut(usize, &str) -> bool,
    ) -> Result<(), String> {
        let to = from.saturating_add(count).min(self.count);
        if from >= to {
            return Ok(());
        }
        let mut line = 0usize;
        // ピースを付け替えないので、位置は前から数える。ピースの数は編集の
        // かたまりの数程度で、行数には比例しない。
        for index in 0..self.pieces.len() {
            let len = self.pieces[index].len();
            let (start, end) = (line, line + len);
            line = end;
            if end <= from {
                continue;
            }
            if start >= to {
                break;
            }
            let skip = from.saturating_sub(start);
            let take = to.min(end) - (start + skip);
            match &self.pieces[index] {
                Piece::Fresh(lines) => {
                    for (i, text) in lines[skip..skip + take].iter().enumerate() {
                        if !f(start + skip + i, text) {
                            return Ok(());
                        }
                    }
                }
                Piece::Disk { from: disk, .. } => {
                    let disk = *disk + skip;
                    let base = start + skip;
                    let mut go = true;
                    let source = self
                        .source
                        .as_mut()
                        .expect("ディスクのピースがあるなら開いたファイルもある");
                    source.each_line(disk, take, &mut |at, text| {
                        go = f(base + (at - disk), text);
                        go
                    })?;
                    if !go {
                        return Ok(());
                    }
                }
            }
        }
        Ok(())
    }

    pub fn read(&mut self, from: usize, count: usize) -> Result<Vec<String>, String> {
        let mut lines = Vec::new();
        self.each_line(from, count, &mut |_, text| {
            lines.push(text.to_string());
            true
        })?;
        Ok(lines)
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
        if from > to || to > self.count {
            return Err("置き換えの範囲が文書の外です".to_string());
        }
        let edit = self.splice(from, to, lines)?;
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
        Ok(self.count)
    }

    /// 置き換えの本体。履歴には触らず、逆操作を返す。取り除く行の中身は
    /// ここでディスクから控える（元に戻すために要る）。
    fn splice(&mut self, from: usize, to: usize, lines: Vec<String>) -> Result<Edit, String> {
        let removed = self.read(from, to - from)?;
        let a = self.split(from);
        let b = self.split(to);
        let inserted = lines.len();
        let fresh = (!lines.is_empty()).then_some(Piece::Fresh(lines));
        self.pieces.splice(a..b, fresh);
        self.count = self.count - removed.len() + inserted;
        if self.count == 0 {
            // 文書は少なくとも 1 行。frontend のモデルも空文書を 1 行と数える。
            self.pieces.push(Piece::Fresh(vec![String::new()]));
            self.count = 1;
        }
        Ok(Edit {
            from,
            removed,
            inserted,
        })
    }

    /// ピース列を行 `line` の前で切り、その位置のピース番号を返す。
    fn split(&mut self, line: usize) -> usize {
        let mut start = 0;
        for index in 0..self.pieces.len() {
            let len = self.pieces[index].len();
            if line == start {
                return index;
            }
            if line < start + len {
                let offset = line - start;
                let tail = match &mut self.pieces[index] {
                    Piece::Disk { from, lines } => {
                        let tail = Piece::Disk {
                            from: *from + offset,
                            lines: *lines - offset,
                        };
                        *lines = offset;
                        tail
                    }
                    Piece::Fresh(lines) => Piece::Fresh(lines.split_off(offset)),
                };
                self.pieces.insert(index + 1, tail);
                return index + 1;
            }
            start += len;
        }
        self.pieces.len()
    }

    pub fn undo(&mut self) -> Result<Option<Restored>, String> {
        let Some(step) = self.undo.pop() else {
            return Ok(None);
        };
        let (reverted, touched_from) = self.revert(&step)?;
        let state = step.before.clone();
        self.redo.push(reverted);
        Ok(Some(Restored {
            state,
            touched_from,
            line_count: self.count,
        }))
    }

    pub fn redo(&mut self) -> Result<Option<Restored>, String> {
        let Some(step) = self.redo.pop() else {
            return Ok(None);
        };
        let (reverted, touched_from) = self.revert(&step)?;
        let state = step.after.clone();
        self.undo.push(reverted);
        Ok(Some(Restored {
            state,
            touched_from,
            line_count: self.count,
        }))
    }

    /// ステップの置き換えを新しい順に巻き戻し、巻き戻し自体を巻き戻すステップを返す。
    fn revert(&mut self, step: &Step) -> Result<(Step, usize), String> {
        let mut inverse = Vec::with_capacity(step.edits.len());
        let mut touched_from = usize::MAX;
        for edit in step.edits.iter().rev() {
            touched_from = touched_from.min(edit.from);
            inverse.push(self.splice(
                edit.from,
                edit.from + edit.inserted,
                edit.removed.clone(),
            )?);
        }
        // 巻き戻しの逆は元のステップと同じ向き。適用した順の逆で持つ。
        Ok((
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
        ))
    }

    /// 検索の走査で見つかったもの: 素の行の一致か、読み替え（記法の解釈）を
    /// 要する行。行の順に並ぶ。
    pub fn scan(
        &mut self,
        pattern: &regex::Regex,
        needle: char,
        from: usize,
        count: usize,
        limit: usize,
    ) -> Result<(Vec<ScanHit>, usize), String> {
        let mut hits = Vec::new();
        let mut scanned_to = from.saturating_add(count).min(self.count);
        self.each_line(from, count, &mut |i, line| {
            if line.contains(needle) {
                // 記法を含む行の一致はこちらでは判定できない。frontend が
                // 行を取り寄せて構造ごと検索する。
                hits.push(ScanHit {
                    line: i,
                    notation: true,
                    start: 0,
                    end: 0,
                });
            } else {
                for found in pattern.find_iter(line) {
                    let start = line[..found.start()].chars().count();
                    let end = start + line[found.start()..found.end()].chars().count();
                    hits.push(ScanHit {
                        line: i,
                        notation: false,
                        start,
                        end,
                    });
                }
            }
            if hits.len() >= limit {
                scanned_to = i + 1;
                return false;
            }
            true
        })?;
        Ok((hits, scanned_to))
    }

    /// `from..=to` の行のうち `needle` を含むもの。
    pub fn lines_containing(
        &mut self,
        from: usize,
        to: usize,
        needle: char,
    ) -> Result<Vec<usize>, String> {
        let to = to.min(self.count.saturating_sub(1));
        let mut found = Vec::new();
        self.each_line(from, to.saturating_sub(from) + 1, &mut |i, line| {
            if line.contains(needle) {
                found.push(i);
            }
            true
        })?;
        Ok(found)
    }

    /// 選択された範囲をひとつなぎのテキストにする。`first` / `last` は端の行の
    /// 切り出し（`None` なら行を丸ごと）。`overrides` の行は差し替えて使う。
    pub fn assemble(
        &mut self,
        from: usize,
        first: Option<String>,
        to: usize,
        last: Option<String>,
        overrides: &std::collections::HashMap<usize, String>,
    ) -> Result<String, String> {
        if from > to || to >= self.count {
            return Err("コピーの範囲が文書の外です".to_string());
        }
        let mut out = String::new();
        self.each_line(from, to - from + 1, &mut |i, line| {
            if i > from {
                out.push('\n');
            }
            if i == from && first.is_some() {
                out.push_str(first.as_deref().unwrap_or_default());
            } else if i == to && last.is_some() {
                out.push_str(last.as_deref().unwrap_or_default());
            } else {
                out.push_str(overrides.get(&i).map(String::as_str).unwrap_or(line));
            }
            true
        })?;
        Ok(out)
    }

    /// 文書の行を書き手へ流す。全文を 1 つの文字列に集めない。
    pub fn write_to<W: Write>(&mut self, out: &mut W) -> Result<(), String> {
        let mut broken = None;
        self.each_line(0, self.count, &mut |i, line| {
            let write = |out: &mut W| -> std::io::Result<()> {
                if i > 0 {
                    out.write_all(b"\n")?;
                }
                out.write_all(line.as_bytes())
            };
            match write(out) {
                Ok(()) => true,
                Err(e) => {
                    broken = Some(format!("書き込めませんでした: {e}"));
                    false
                }
            }
        })?;
        match broken {
            Some(error) => Err(error),
            None => out
                .flush()
                .map_err(|e| format!("書き込めませんでした: {e}")),
        }
    }

    /// 文書をディスクへ流し、保存したファイルを新しい本体にする。
    /// 一時ファイルへ書いてから入れ替えるので、書きかけで元を壊さない。
    pub fn save(&mut self, path: &str) -> Result<(), String> {
        let tmp = format!("{path}.saving");
        let fail = |e: String| format!("{path} を保存できませんでした: {e}");
        // 書きながら次の索引を作る。保存が終わった姿はこの索引そのもの。
        let mut marks = vec![0u64];
        let mut written = 0u64;
        {
            let file = File::create(&tmp).map_err(|e| fail(e.to_string()))?;
            let mut out = BufWriter::with_capacity(CHUNK, file);
            let count = self.count;
            let mut broken = None;
            self.each_line(0, count, &mut |i, line| {
                let mut write = |out: &mut BufWriter<File>| -> std::io::Result<()> {
                    if i > 0 {
                        out.write_all(b"\n")?;
                        written += 1;
                        if i % STRIDE == 0 {
                            marks.push(written);
                        }
                    }
                    out.write_all(line.as_bytes())?;
                    written += line.len() as u64;
                    Ok(())
                };
                match write(&mut out) {
                    Ok(()) => true,
                    Err(e) => {
                        broken = Some(e.to_string());
                        false
                    }
                }
            })?;
            if let Some(error) = broken {
                std::fs::remove_file(&tmp).ok();
                return Err(fail(error));
            }
            out.flush().map_err(|e| fail(e.to_string()))?;
        }
        // 自分が読んでいる元ファイルへ重ねるかもしれないので、先に手を放す。
        if self
            .source
            .as_ref()
            .is_some_and(|source| source.path == Path::new(path))
        {
            self.source = None;
        }
        std::fs::rename(&tmp, path).map_err(|e| fail(e.to_string()))?;
        let file = File::open(path).map_err(|e| fail(e.to_string()))?;
        let modified = file.metadata().ok().and_then(|meta| meta.modified().ok());
        self.source = Some(Source {
            path: Path::new(path).to_path_buf(),
            file,
            marks,
            lines: self.count,
            bytes: written,
            modified,
        });
        self.pieces = vec![Piece::Disk {
            from: 0,
            lines: self.count,
        }];
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一意な一時ファイルに行を書き、開いた文書とパスを返す。
    fn disk_doc(name: &str, lines: &[&str]) -> (Document, String) {
        let path = std::env::temp_dir().join(format!(
            "planetext-store-{}-{}.txt",
            std::process::id(),
            name
        ));
        let path = path.to_string_lossy().into_owned();
        std::fs::write(&path, lines.join("\n")).unwrap();
        (Document::open(&path).unwrap(), path)
    }

    fn all(doc: &mut Document) -> Vec<String> {
        doc.read(0, usize::MAX).unwrap()
    }

    #[test]
    fn opening_indexes_lines_without_holding_the_contents() {
        let (mut doc, path) = disk_doc("open", &["ab", "", "cd"]);
        assert_eq!(doc.line_count(), 3);
        assert_eq!(all(&mut doc), vec!["ab", "", "cd"]);
        assert_eq!(doc.read(2, 1).unwrap(), vec!["cd"]);
        std::fs::remove_file(path).ok();
    }

    /// 間引きの索引: STRIDE を越える文書でも、行は最寄りの索引から読み流して届く。
    #[test]
    fn far_lines_are_reached_through_the_sparse_index() {
        let lines: Vec<String> = (0..STRIDE * 2 + 5).map(|i| format!("line {i}")).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let (mut doc, path) = disk_doc("stride", &refs);
        assert_eq!(doc.line_count(), STRIDE * 2 + 5);
        assert_eq!(
            doc.read(STRIDE + 3, 2).unwrap(),
            vec![
                format!("line {}", STRIDE + 3),
                format!("line {}", STRIDE + 4)
            ]
        );
        assert_eq!(
            doc.read(STRIDE * 2 + 4, 5).unwrap(),
            vec![format!("line {}", STRIDE * 2 + 4)]
        );
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn replacing_lines_and_undoing_restores_the_document() {
        let (mut doc, path) = disk_doc("undo", &["a", "b", "c"]);
        doc.replace(1, 2, vec!["X".into(), "Y".into()], 1, "before", "after")
            .unwrap();
        assert_eq!(all(&mut doc), vec!["a", "X", "Y", "c"]);
        let undone = doc.undo().unwrap().unwrap();
        assert_eq!(undone.state, "before");
        assert_eq!(undone.touched_from, 1);
        assert_eq!(all(&mut doc), vec!["a", "b", "c"]);
        let redone = doc.redo().unwrap().unwrap();
        assert_eq!(redone.state, "after");
        assert_eq!(all(&mut doc), vec!["a", "X", "Y", "c"]);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn steps_in_the_same_group_undo_together() {
        let (mut doc, path) = disk_doc("group", &["a", "b", "c"]);
        // 「すべて置換」のように、複数の置き換えが 1 つのグループで届く。
        doc.replace(2, 3, vec!["C".into()], 7, "start", "mid")
            .unwrap();
        doc.replace(0, 1, vec!["A".into()], 7, "ignored", "end")
            .unwrap();
        assert_eq!(all(&mut doc), vec!["A", "b", "C"]);
        let undone = doc.undo().unwrap().unwrap();
        assert_eq!(all(&mut doc), vec!["a", "b", "c"]);
        assert_eq!(undone.state, "start");
        assert_eq!(undone.touched_from, 0);
        assert!(doc.undo().unwrap().is_none());
        let redone = doc.redo().unwrap().unwrap();
        assert_eq!(all(&mut doc), vec!["A", "b", "C"]);
        assert_eq!(redone.state, "end");
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn different_groups_undo_one_at_a_time() {
        let mut doc = Document::empty();
        doc.replace(0, 1, vec!["b".into()], 1, "s1", "e1").unwrap();
        doc.replace(0, 1, vec!["c".into()], 2, "s2", "e2").unwrap();
        doc.undo().unwrap();
        assert_eq!(all(&mut doc), vec!["b"]);
        doc.undo().unwrap();
        assert_eq!(all(&mut doc), vec![""]);
    }

    #[test]
    fn a_new_edit_clears_the_redo_branch() {
        let mut doc = Document::empty();
        doc.replace(0, 1, vec!["b".into()], 1, "", "").unwrap();
        doc.undo().unwrap();
        doc.replace(0, 1, vec!["c".into()], 2, "", "").unwrap();
        assert!(doc.redo().unwrap().is_none());
        assert_eq!(all(&mut doc), vec!["c"]);
    }

    #[test]
    fn out_of_range_replacements_are_rejected() {
        let mut doc = Document::empty();
        assert!(doc.replace(0, 2, vec![], 1, "", "").is_err());
        assert!(doc.replace(1, 0, vec![], 1, "", "").is_err());
    }

    #[test]
    fn assembling_a_range_uses_edges_and_overrides() {
        let (mut doc, path) = disk_doc("assemble", &["aa", "bb", "cc", "dd"]);
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
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn scanning_reports_matches_and_notation_lines_in_order() {
        let (mut doc, path) = disk_doc("scan", &["fox", "a$b", "the fox and fox", "last"]);
        let pattern = regex::Regex::new("fox").unwrap();
        let (hits, scanned_to) = doc.scan(&pattern, '$', 0, 10, 64).unwrap();
        assert_eq!(scanned_to, 4);
        let kinds: Vec<(usize, bool, usize, usize)> = hits
            .iter()
            .map(|hit| (hit.line, hit.notation, hit.start, hit.end))
            .collect();
        assert_eq!(
            kinds,
            vec![
                (0, false, 0, 3),
                (1, true, 0, 0),
                (2, false, 4, 7),
                (2, false, 12, 15),
            ]
        );
        // 途中から頼めば続きが返る。
        let (hits, _) = doc.scan(&pattern, '$', 2, 10, 64).unwrap();
        assert_eq!(hits.len(), 2);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn finding_lines_that_contain_a_character() {
        let (mut doc, path) = disk_doc("contains", &["aa", "a$b", "cc", "$"]);
        assert_eq!(doc.lines_containing(0, 3, '$').unwrap(), vec![1, 3]);
        assert_eq!(doc.lines_containing(0, 99, '$').unwrap(), vec![1, 3]);
        std::fs::remove_file(path).ok();
    }

    /// 保存すると保存先が新しい本体になり、続きの読みも元に戻すも生きている。
    #[test]
    fn saving_adopts_the_written_file_as_the_source() {
        let (mut doc, path) = disk_doc("save", &["a", "b"]);
        doc.replace(1, 2, vec!["B".into()], 1, "before", "")
            .unwrap();
        doc.save(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a\nB");
        assert_eq!(all(&mut doc), vec!["a", "B"]);
        let undone = doc.undo().unwrap().unwrap();
        assert_eq!(undone.state, "before");
        assert_eq!(all(&mut doc), vec!["a", "b"]);
        std::fs::remove_file(path).ok();
    }

    /// 開いている間の外部変更は、壊れた読みを返さずに断る。
    #[test]
    fn outside_changes_are_refused_instead_of_read_wrong() {
        let (mut doc, path) = disk_doc("outside", &["a", "b"]);
        std::fs::write(&path, "changed elsewhere\nlonger than before").unwrap();
        assert!(doc.read(0, 2).is_err());
        std::fs::remove_file(path).ok();
    }

    /// 規模の実測: `cargo test -p planetext --release -- --ignored --nocapture`。
    /// C:\workspace\test-800mb.txt がある環境でだけ動く。
    #[test]
    #[ignore]
    fn scale_check_opening_a_huge_file() {
        let path = r"C:\workspace\test-800mb.txt";
        if !std::path::Path::new(path).exists() {
            return;
        }
        let start = std::time::Instant::now();
        let mut doc = Document::open(path).unwrap();
        println!(
            "open (index {} lines): {:?}",
            doc.line_count(),
            start.elapsed()
        );
        let start = std::time::Instant::now();
        let middle = doc.read(doc.line_count() / 2, 100).unwrap();
        println!(
            "read 100 lines: {:?} ({} lines)",
            start.elapsed(),
            middle.len()
        );
    }
}
