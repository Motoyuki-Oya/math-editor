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
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

/// 何行ごとに行頭のバイト位置を控えるか。1600 万行でも索引は数万個で済み、
/// どの行へも高々この行数だけ読み流せば届く。
const STRIDE: usize = 1024;

/// 履歴が持つステップ数の上限。それより古いものは忘れる。
const HISTORY_LIMIT: usize = 1000;

/// ファイルを読むときのひとかたまり。
const CHUNK: usize = 1 << 20;

/// チャンク内の改行を数え、STRIDE 行ごとの行頭バイト位置を `marks` へ足す。
/// `line` は通し行数、`offset` はチャンク先頭のファイル内バイト位置。
fn index_chunk(chunk: &[u8], offset: u64, line: &mut usize, marks: &mut Vec<u64>) {
    // memchrのcountはSIMD実装を持つ。4KBごとに改行数だけ先に数え、STRIDEの
    // 境界を含む小区間だけ位置を列挙することで、debugでも全改行をRust側で
    // 1件ずつ反復しない。
    const BLOCK: usize = 4 << 10;
    for (block_index, block) in chunk.chunks(BLOCK).enumerate() {
        let count = memchr::memchr_iter(b'\n', block).count();
        let next_mark = (*line / STRIDE + 1) * STRIDE;
        if *line + count < next_mark {
            *line += count;
            continue;
        }
        for at in memchr::memchr_iter(b'\n', block) {
            *line += 1;
            if (*line).is_multiple_of(STRIDE) {
                marks.push(offset + (block_index * BLOCK + at) as u64 + 1);
            }
        }
    }
}

/// 通常文字列の一致バイト位置。ASCIIの大小無視は先頭バイト候補だけを調べ、
/// 候補ごとにASCII case-foldで比較する。結果はregexと同じ非重複一致。
fn literal_positions(haystack: &[u8], needle: &[u8], case_sensitive: bool) -> Vec<usize> {
    if needle.is_empty() {
        return Vec::new();
    }
    if case_sensitive {
        return memchr::memmem::find_iter(haystack, needle).collect();
    }
    // 先頭文字が頻出すると候補比較だけで遅くなる。記号を最優先し、文字なら
    // 英文で比較的まれな文字を錨にして、その位置から一致の先頭を逆算する。
    let rarity = |byte: u8| match byte.to_ascii_lowercase() {
        b'e' => 12,
        b't' => 11,
        b'a' | b'o' | b'i' => 10,
        b'n' | b's' | b'h' | b'r' => 9,
        b'd' | b'l' => 8,
        b'c' | b'u' | b'm' | b'w' | b'f' | b'g' | b'y' | b'p' | b'b' => 6,
        b'v' | b'k' | b'j' | b'x' | b'q' | b'z' => 3,
        byte if byte.is_ascii_alphanumeric() => 5,
        _ => 0,
    };
    let anchor = (0..needle.len())
        .min_by_key(|index| rarity(needle[*index]))
        .unwrap_or(0);
    let byte = needle[anchor];
    let lower = byte.to_ascii_lowercase();
    let upper = byte.to_ascii_uppercase();
    let candidates: Box<dyn Iterator<Item = usize> + '_> = if lower == upper {
        Box::new(memchr::memchr_iter(byte, haystack))
    } else {
        Box::new(memchr::memchr2_iter(lower, upper, haystack))
    };
    let mut next = 0usize;
    candidates
        .filter_map(|at| {
            let start = at.checked_sub(anchor)?;
            if start < next || start + needle.len() > haystack.len() {
                return None;
            }
            if haystack[start..start + needle.len()].eq_ignore_ascii_case(needle) {
                next = start + needle.len();
                Some(start)
            } else {
                None
            }
        })
        .collect()
}

/// 走査スレッドが更新する間引き索引と行数。
pub struct ScanIndex {
    state: Mutex<ScanState>,
}

struct ScanState {
    /// STRIDE 行ごとの行頭のバイト位置。先頭は必ず 0。
    marks: Vec<u64>,
    /// 走査済みの行数。完了後は総行数。
    lines: usize,
    /// 走査が終わったか。
    done: bool,
    /// 途中で UTF-8 違反が見つかった場合の遅延エラー。
    broken: Option<String>,
}

impl ScanIndex {
    /// `Ok(Some(lines))` は完了、`Ok(None)` は走査中、`Err` は遅延エラー。
    pub fn status(&self) -> Result<Option<usize>, String> {
        let index = self.state.lock().unwrap();
        match &index.broken {
            Some(error) => Err(error.clone()),
            None if index.done => Ok(Some(index.lines)),
            None => Ok(None),
        }
    }
}

/// 開いたファイル。行の中身はここから seek で読む。
struct Source {
    path: PathBuf,
    file: File,
    /// 行数と間引き索引はバックグラウンド走査スレッドと共有する。
    index: Arc<ScanIndex>,
    bytes: u64,
    /// 開いたときの姿。外から書き換えられると seek 読みが壊れるので、
    /// 変わっていたら読む前に断る。
    modified: Option<SystemTime>,
}

pub struct BackgroundScan {
    reader: BufReader<File>,
    index: Arc<ScanIndex>,
    path: PathBuf,
    offset: u64,
    line: usize,
    carry: Vec<u8>,
}

impl BackgroundScan {
    pub fn run(mut self) -> Result<Option<usize>, String> {
        loop {
            if Arc::strong_count(&self.index) == 1 {
                return Ok(None);
            }
            let chunk = self
                .reader
                .fill_buf()
                .map_err(|e| format!("{} を読めませんでした: {e}", self.path.display()))?;
            if chunk.is_empty() {
                if !self.carry.is_empty() {
                    let error = format!("{} は UTF-8 ではありません", self.path.display());
                    self.index.state.lock().unwrap().broken = Some(error.clone());
                    return Err(error);
                }
                let mut index = self.index.state.lock().unwrap();
                index.lines = self.line + 1;
                index.done = true;
                return Ok(Some(index.lines));
            }
            let mut marks = Vec::new();
            index_chunk(chunk, self.offset, &mut self.line, &mut marks);
            if !valid_utf8(&mut self.carry, chunk) {
                let error = format!("{} は UTF-8 ではありません", self.path.display());
                self.index.state.lock().unwrap().broken = Some(error.clone());
                return Err(error);
            }
            let len = chunk.len();
            self.offset += len as u64;
            self.reader.consume(len);
            let mut index = self.index.state.lock().unwrap();
            index.marks.extend(marks);
            index.lines = self.line;
        }
    }
}

impl Source {
    /// 先頭 1MB だけ読み、索引を作り、残りを BackgroundScan に任せる。
    fn open(path: &Path) -> Result<(Source, Option<BackgroundScan>), String> {
        let file =
            File::open(path).map_err(|e| format!("{} を開けませんでした: {e}", path.display()))?;
        let modified = file.metadata().ok().and_then(|meta| meta.modified().ok());
        let bytes = file.metadata().ok().map_or(0, |m| m.len());
        // try_clone はカーソルを共有し、読みの seek が走査の位置を壊すので、
        // 走査には独立したハンドルを開く。
        let scan_file =
            File::open(path).map_err(|e| format!("{} を読めませんでした: {e}", path.display()))?;
        let mut reader = BufReader::with_capacity(CHUNK, scan_file);
        let index = Arc::new(ScanIndex {
            state: Mutex::new(ScanState {
                marks: vec![0],
                lines: 0,
                done: false,
                broken: None,
            }),
        });
        let mut marks = vec![0u64];
        let mut line = 0usize;
        let mut offset = 0u64;
        let mut carry: Vec<u8> = Vec::new();
        {
            let chunk = reader
                .fill_buf()
                .map_err(|e| format!("{} を読めませんでした: {e}", path.display()))?;
            index_chunk(chunk, offset, &mut line, &mut marks);
            if !valid_utf8(&mut carry, chunk) {
                return Err(format!("{} は UTF-8 ではありません", path.display()));
            }
            let len = chunk.len();
            offset += len as u64;
            reader.consume(len);
        }
        // 最初のチャンクでファイル全体を読めたなら、行数はここで確定する。
        let done = offset >= bytes;
        if done && !carry.is_empty() {
            return Err(format!("{} は UTF-8 ではありません", path.display()));
        }

        {
            let mut state = index.state.lock().unwrap();
            state.marks = marks;
            state.lines = if done { line + 1 } else { line };
            state.done = done;
        }

        let source = Source {
            path: path.to_path_buf(),
            file,
            index: index.clone(),
            bytes,
            modified,
        };

        let scan = if done {
            drop(reader);
            None
        } else {
            Some(BackgroundScan {
                reader,
                index,
                path: path.to_path_buf(),
                offset,
                line,
                carry,
            })
        };

        Ok((source, scan))
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

    fn lines(&self) -> usize {
        self.index.state.lock().unwrap().lines
    }

    /// 総行数や間引き索引を待たず、EOFから後ろの行だけを読む。
    fn read_tail(&mut self, count: usize) -> Result<Vec<String>, String> {
        if count == 0 || self.bytes == 0 {
            return Ok(Vec::new());
        }
        self.check()?;
        const TAIL_CHUNK: u64 = 64 << 10;
        const MAX_TAIL_BYTES: u64 = 8 << 20;
        let mut at = self.bytes;
        let mut chunks = Vec::new();
        let mut newlines = 0usize;
        let mut read_bytes = 0u64;
        while at > 0 && newlines < count && read_bytes < MAX_TAIL_BYTES {
            let from = at.saturating_sub(TAIL_CHUNK);
            let mut chunk = vec![0; (at - from) as usize];
            self.file
                .seek(SeekFrom::Start(from))
                .map_err(|e| format!("{} を読めませんでした: {e}", self.path.display()))?;
            self.file
                .read_exact(&mut chunk)
                .map_err(|e| format!("{} を読めませんでした: {e}", self.path.display()))?;
            newlines += memchr::memchr_iter(b'\n', &chunk).count();
            read_bytes += chunk.len() as u64;
            chunks.push(chunk);
            at = from;
        }
        if at > 0 && newlines < count {
            return Err("末尾の行が8MBを超えるため表示できません".to_string());
        }
        chunks.reverse();
        let bytes: Vec<u8> = chunks.into_iter().flatten().collect();
        let lines: Vec<&[u8]> = bytes.split(|byte| *byte == b'\n').collect();
        let first = lines.len().saturating_sub(count);
        lines[first..]
            .iter()
            .map(|line| {
                std::str::from_utf8(line)
                    .map(str::to_string)
                    .map_err(|_| format!("{} は UTF-8 ではありません", self.path.display()))
            })
            .collect()
    }

    /// 検索スレッド専用の独立したファイルハンドルを持つ同じソース。
    fn search_copy(&self) -> Result<Source, String> {
        self.check()?;
        let file = File::open(&self.path)
            .map_err(|e| format!("{} を読めませんでした: {e}", self.path.display()))?;
        Ok(Source {
            path: self.path.clone(),
            file,
            index: self.index.clone(),
            bytes: self.bytes,
            modified: self.modified,
        })
    }

    /// ディスク上の行範囲をひとかたまりで読み、通常文字列を memmem で探す。
    /// 行ごとの read_until を避け、一致した場所だけ文字の列位置へ変換する。
    fn literal_matches(
        &mut self,
        from: usize,
        count: usize,
        query: &str,
        case_sensitive: bool,
        marker: u8,
    ) -> Result<Vec<ScanHit>, String> {
        if count == 0 || query.is_empty() {
            return Ok(Vec::new());
        }
        self.check()?;
        let lines = self.lines();
        let to = from.saturating_add(count).min(lines);
        if from >= to {
            return Ok(Vec::new());
        }
        let (base, start, end) = {
            let state = self.index.state.lock().unwrap();
            let group = (from / STRIDE).min(state.marks.len().saturating_sub(1));
            let end_group = to.div_ceil(STRIDE);
            (
                group * STRIDE,
                state.marks[group],
                state.marks.get(end_group).copied().unwrap_or(self.bytes),
            )
        };
        let mut bytes = vec![0; (end - start) as usize];
        let broken = |e| format!("{} を読めませんでした: {e}", self.path.display());
        self.file.seek(SeekFrom::Start(start)).map_err(broken)?;
        self.file.read_exact(&mut bytes).map_err(broken)?;
        let match_bytes = literal_positions(&bytes, query.as_bytes(), case_sensitive);
        let marker_bytes: Vec<usize> = memchr::memchr_iter(marker, &bytes).collect();
        // 候補が無ければ行番号へ直す必要もない。巨大な改行配列を作らず返る。
        if match_bytes.is_empty() && marker_bytes.is_empty() {
            return Ok(Vec::new());
        }
        let newlines: Vec<usize> = memchr::memchr_iter(b'\n', &bytes).collect();
        let line_at = |byte: usize| base + newlines.partition_point(|newline| *newline < byte);
        let mut marked = std::collections::HashSet::new();
        for byte in marker_bytes {
            let line = line_at(byte);
            if line >= from && line < to {
                marked.insert(line);
            }
        }
        let mut hits: Vec<ScanHit> = marked
            .iter()
            .map(|line| ScanHit {
                line: *line,
                notation: true,
                start: 0,
                end: 0,
            })
            .collect();
        for byte in match_bytes {
            let line = line_at(byte);
            if line < from || line >= to || marked.contains(&line) {
                continue;
            }
            let end_byte = byte + query.len();
            if bytes[byte..end_byte].contains(&b'\n') {
                continue;
            }
            let line_start = (line - base)
                .checked_sub(1)
                .and_then(|index| newlines.get(index))
                .map_or(0, |newline| newline + 1);
            let start_col = std::str::from_utf8(&bytes[line_start..byte])
                .map_err(|_| format!("{} は UTF-8 ではありません", self.path.display()))?
                .chars()
                .count();
            let width = query.chars().count();
            hits.push(ScanHit {
                line,
                notation: false,
                start: start_col,
                end: start_col + width,
            });
        }
        hits.sort_by_key(|hit| (hit.line, hit.start));
        Ok(hits)
    }

    /// ディスク上の行 `from` から `count` 行に `f` を呼ぶ。`f` が `false` を
    /// 返したら打ち切る。最寄りの索引へ seek し、そこから読み流す。
    fn each_line(
        &mut self,
        from: usize,
        count: usize,
        f: &mut dyn FnMut(usize, &str) -> bool,
    ) -> Result<(), String> {
        if count == 0 {
            return Ok(());
        }
        let (lines, mark_index, mark) = {
            let state = self.index.state.lock().unwrap();
            if let Some(error) = &state.broken {
                return Err(error.clone());
            }
            let mark_index = (from / STRIDE).min(state.marks.len().saturating_sub(1));
            (state.lines, mark_index, state.marks[mark_index])
        };
        if from >= lines {
            return Ok(());
        }
        self.check()?;
        let broken = |e| format!("{} を読めませんでした: {e}", self.path.display());
        self.file.seek(SeekFrom::Start(mark)).map_err(broken)?;
        let mut reader = BufReader::with_capacity(CHUNK, &self.file);
        let mut buffer = Vec::new();
        // 遅延索引が目的行へまだ届いていなければ、最後にある印から正しく読む。
        for _ in 0..from - mark_index * STRIDE {
            buffer.clear();
            reader.read_until(b'\n', &mut buffer).map_err(broken)?;
        }
        let to = (from + count).min(lines);
        for line in from..to {
            buffer.clear();
            reader.read_until(b'\n', &mut buffer).map_err(broken)?;
            if buffer.last() == Some(&b'\n') {
                buffer.pop();
            }
            let text = std::str::from_utf8(&buffer)
                .map_err(|_| format!("{} は UTF-8 ではありません", self.path.display()))?;
            if !f(line, text) {
                return Ok(());
            }
        }
        Ok(())
    }
}

/// チャンクの並びが UTF-8 かどうか。文字の途中でチャンクが切れることがある
/// ので、切れ端は `carry` に持ち越して次のチャンクの頭と合わせて見る。
fn valid_utf8(carry: &mut Vec<u8>, mut chunk: &[u8]) -> bool {
    // 巨大なログや生成テキストの大半はASCII。sliceのword-at-a-time判定で
    // 通れば、汎用UTF-8検証をもう一度全バイトへ掛けない。
    if carry.is_empty() && chunk.is_ascii() {
        return true;
    }
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
#[derive(Clone, Debug)]
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

pub struct SearchCandidates {
    pub hits: Vec<ScanHit>,
    pub scanned_to: usize,
    pub cancelled: bool,
}

pub struct SearchSpec<'a> {
    pub pattern: &'a regex::Regex,
    pub literal: Option<&'a str>,
    pub case_sensitive: bool,
    pub marker: char,
    pub from: usize,
    pub end: usize,
    pub after_col: Option<usize>,
}

impl Document {
    pub fn open(path: &str) -> Result<(Document, Option<BackgroundScan>), String> {
        let (source, scan) = Source::open(Path::new(path))?;
        let count = source.lines();
        Ok((
            Document {
                pieces: vec![Piece::Disk {
                    from: 0,
                    lines: count,
                }],
                count,
                source: Some(source),
                undo: Vec::new(),
                redo: Vec::new(),
            },
            scan,
        ))
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

    pub fn scan_index(&self) -> Option<Arc<ScanIndex>> {
        self.source.as_ref().map(|source| source.index.clone())
    }

    /// 検索スレッドへ渡す読み取り専用の姿。ファイルカーソルは独立し、編集の
    /// ピースは開始時点の内容を複製するので、文書ロックを持たずに走査できる。
    pub fn search_snapshot(&self) -> Result<Document, String> {
        Ok(Document {
            source: self.source.as_ref().map(Source::search_copy).transpose()?,
            pieces: self.pieces.clone(),
            count: self.count,
            undo: Vec::new(),
            redo: Vec::new(),
        })
    }

    /// 走査完了後に呼ぶ。ディスクのピースと行数を確定値へ合わせる。
    pub fn confirm_scan(&mut self) {
        let Some(source) = self.source.as_ref() else {
            return;
        };
        let exact = source.lines();
        if let Some(Piece::Disk { from, lines }) = self.pieces.last_mut() {
            *lines = exact.saturating_sub(*from);
        }
        self.count = self.pieces.iter().map(Piece::len).sum();
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
                    let source = self.source.as_mut().ok_or_else(|| {
                        "文書ストアのディスク参照が失われました。開き直してください".to_string()
                    })?;
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

    pub fn read_tail(&mut self, count: usize) -> Result<Vec<String>, String> {
        let scan_done = self
            .source
            .as_ref()
            .and_then(|source| source.index.status().ok().flatten())
            .is_some();
        if scan_done {
            self.confirm_scan();
            return self.read(self.count.saturating_sub(count), count);
        }
        self.source
            .as_mut()
            .ok_or_else(|| "末尾を読むファイルがありません".to_string())?
            .read_tail(count)
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

    /// `from..end` を native 内で連続走査し、最初の候補群まで進む。空の
    /// ページを frontend と往復せず、ページごとにキャンセルを確認する。
    pub fn search_candidates(
        &mut self,
        spec: SearchSpec<'_>,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<SearchCandidates, String> {
        // 1回の読みを大きくしてseek・確保を減らしつつ、キャンセル確認は
        // 100万行ごとに行う（800MBの実測では1ページ数百ms）。
        const PAGE_LINES: usize = 1_000_000;
        let SearchSpec {
            pattern,
            literal,
            case_sensitive,
            marker,
            from,
            end,
            after_col,
        } = spec;
        let end = end.min(self.count);
        let mut at = from.min(end);
        while at < end {
            if cancelled() {
                return Ok(SearchCandidates {
                    hits: Vec::new(),
                    scanned_to: at,
                    cancelled: true,
                });
            }
            let page_end = (at + PAGE_LINES).min(end);
            let (mut hits, _) = if let Some(query) = literal {
                self.scan_literal(query, case_sensitive, marker, at, page_end - at, usize::MAX)?
            } else {
                self.scan(pattern, marker, at, page_end - at, usize::MAX)?
            };
            if at == from {
                if let Some(col) = after_col {
                    hits.retain(|hit| hit.notation || hit.line > from || hit.start >= col);
                }
            }
            if !hits.is_empty() {
                hits.truncate(64);
                let scanned_to = hits.last().map_or(at, |hit| hit.line + 1);
                return Ok(SearchCandidates {
                    hits,
                    scanned_to,
                    cancelled: false,
                });
            }
            at = page_end;
        }
        Ok(SearchCandidates {
            hits: Vec::new(),
            scanned_to: end,
            cancelled: false,
        })
    }

    /// 通常の大小区別あり文字列検索。ディスクのピースはバイト範囲をまとめて
    /// memmem で探し、編集で入った行も同じ結果形式へ合わせる。
    pub fn scan_literal(
        &mut self,
        query: &str,
        case_sensitive: bool,
        marker: char,
        from: usize,
        count: usize,
        limit: usize,
    ) -> Result<(Vec<ScanHit>, usize), String> {
        let to = from.saturating_add(count).min(self.count);
        let mut hits = Vec::new();
        let mut line = 0usize;
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
                    for (offset, text) in lines[skip..skip + take].iter().enumerate() {
                        let at = start + skip + offset;
                        if text.contains(marker) {
                            hits.push(ScanHit {
                                line: at,
                                notation: true,
                                start: 0,
                                end: 0,
                            });
                        } else {
                            for byte in
                                literal_positions(text.as_bytes(), query.as_bytes(), case_sensitive)
                            {
                                let start_col = text[..byte].chars().count();
                                hits.push(ScanHit {
                                    line: at,
                                    notation: false,
                                    start: start_col,
                                    end: start_col + query.chars().count(),
                                });
                            }
                        }
                    }
                }
                Piece::Disk { from: disk, .. } => {
                    let disk = *disk + skip;
                    let base = start + skip;
                    let source = self.source.as_mut().ok_or_else(|| {
                        "文書ストアのディスク参照が失われました。開き直してください".to_string()
                    })?;
                    hits.extend(
                        source
                            .literal_matches(disk, take, query, case_sensitive, marker as u8)?
                            .into_iter()
                            .map(|hit| ScanHit {
                                line: base + (hit.line - disk),
                                ..hit
                            }),
                    );
                }
            }
            if hits.len() >= limit {
                hits.sort_by_key(|hit| (hit.line, hit.start));
                hits.truncate(limit);
                let scanned_to = hits.last().map_or(from, |hit| hit.line + 1);
                return Ok((hits, scanned_to));
            }
        }
        hits.sort_by_key(|hit| (hit.line, hit.start));
        hits.truncate(limit);
        Ok((hits, to))
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

    /// 文書から等間隔の窓を標本として検索し、全文の一致数を推定する。
    /// 小さい文書は全行を調べるので正確な件数になる。
    pub fn estimate_matches(&mut self, pattern: &regex::Regex) -> Result<usize, String> {
        const WINDOWS: usize = 64;
        const LINES_PER_WINDOW: usize = 2_000;
        let step = self.count.div_ceil(WINDOWS).max(1);
        let take = LINES_PER_WINDOW.min(step);
        let mut hits = 0usize;
        let mut sampled = 0usize;
        for from in (0..self.count).step_by(step) {
            let count = take.min(self.count - from);
            self.each_line(from, count, &mut |_, line| {
                hits += pattern.find_iter(line).count();
                sampled += 1;
                true
            })?;
        }
        if sampled == 0 {
            return Ok(0);
        }
        Ok(((hits as u128 * self.count as u128 + sampled as u128 / 2) / sampled as u128) as usize)
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
        // 自分が読んでいる元ファイルへ重ねる場合だけ、rename の直前に手を放す。
        // rename が失敗したら必ず戻す。Disk piece を残したまま Source だけ失うと、
        // その後の読みがパニックし、文書マップの Mutex まで poison される。
        let replacing_source = self
            .source
            .as_ref()
            .is_some_and(|source| source.path == Path::new(path));
        let old_source = replacing_source.then(|| self.source.take()).flatten();
        if let Err(error) = std::fs::rename(&tmp, path) {
            self.source = old_source;
            std::fs::remove_file(&tmp).ok();
            return Err(fail(error.to_string()));
        }
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) => {
                self.source = old_source;
                return Err(fail(error.to_string()));
            }
        };
        let modified = file.metadata().ok().and_then(|meta| meta.modified().ok());
        self.source = Some(Source {
            path: Path::new(path).to_path_buf(),
            file,
            index: Arc::new(ScanIndex {
                state: Mutex::new(ScanState {
                    marks,
                    lines: self.count,
                    done: true,
                    broken: None,
                }),
            }),
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
        let (doc, _) = Document::open(&path).unwrap();
        (doc, path)
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

    #[test]
    fn background_scan_confirms_and_reads_the_empty_final_line() {
        let path = std::env::temp_dir().join(format!(
            "planetext-store-{}-background-final.txt",
            std::process::id()
        ));
        let line = "0123456789\n";
        let complete_lines = 100_000usize;
        std::fs::write(&path, line.repeat(complete_lines)).unwrap();
        let path = path.to_string_lossy().into_owned();
        let (mut doc, scan) = Document::open(&path).unwrap();
        assert!(scan.is_some());
        assert_eq!(doc.read_tail(2).unwrap(), vec!["0123456789", ""]);
        scan.unwrap().run().unwrap();
        doc.confirm_scan();
        assert_eq!(doc.line_count(), complete_lines + 1);
        assert_eq!(doc.read(complete_lines, 1).unwrap(), vec![""]);
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
    fn estimating_matches_is_exact_when_every_line_is_sampled() {
        let (mut doc, path) = disk_doc("estimate", &["hit hit", "none", "hit"]);
        let pattern = regex::Regex::new("hit").unwrap();
        assert_eq!(doc.estimate_matches(&pattern).unwrap(), 3);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn estimating_matches_extrapolates_uniform_samples() {
        let lines: Vec<String> = (0..200_000).map(|_| "hit".to_string()).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let (mut doc, path) = disk_doc("estimate-large", &refs);
        let pattern = regex::Regex::new("hit").unwrap();
        assert_eq!(doc.estimate_matches(&pattern).unwrap(), 200_000);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn ascii_case_fold_finds_non_overlapping_matches() {
        assert_eq!(literal_positions(b"xxAbCaBC", b"abc", false), vec![2, 5]);
        assert_eq!(literal_positions(b"aaaa", b"aa", false), vec![0, 2]);
        assert!(literal_positions(b"ABC", b"abc", true).is_empty());
    }

    #[test]
    fn ascii_case_insensitive_scan_keeps_utf8_columns() {
        let (mut doc, path) = disk_doc("ascii-fold", &["前AbC後aBc"]);
        let (hits, _) = doc.scan_literal("abc", false, '$', 0, 1, 64).unwrap();
        assert_eq!(
            hits.iter()
                .map(|hit| (hit.start, hit.end))
                .collect::<Vec<_>>(),
            vec![(1, 4), (5, 8)]
        );
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn search_snapshot_keeps_the_contents_at_search_start() {
        let (mut doc, path) = disk_doc("search-snapshot", &["before", "tail"]);
        doc.replace(0, 1, vec!["first".into()], 1, "", "").unwrap();
        let mut snapshot = doc.search_snapshot().unwrap();
        doc.replace(0, 1, vec!["second".into()], 2, "", "").unwrap();
        let pattern = regex::Regex::new("first").unwrap();
        let found = snapshot
            .search_candidates(
                SearchSpec {
                    pattern: &pattern,
                    literal: Some("first"),
                    case_sensitive: true,
                    marker: '$',
                    from: 0,
                    end: 2,
                    after_col: None,
                },
                &|| false,
            )
            .unwrap();
        assert_eq!(found.hits.len(), 1);
        assert!(!found.cancelled);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn native_search_stops_when_cancelled() {
        let (doc, path) = disk_doc("search-cancel", &["a", "b"]);
        let mut snapshot = doc.search_snapshot().unwrap();
        let pattern = regex::Regex::new("missing").unwrap();
        let found = snapshot
            .search_candidates(
                SearchSpec {
                    pattern: &pattern,
                    literal: Some("missing"),
                    case_sensitive: true,
                    marker: '$',
                    from: 0,
                    end: 2,
                    after_col: None,
                },
                &|| true,
            )
            .unwrap();
        assert!(found.cancelled);
        assert!(found.hits.is_empty());
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn literal_scan_reports_utf8_character_columns() {
        let (mut doc, path) = disk_doc("literal-scan", &["前abc後abc"]);
        let (hits, _) = doc.scan_literal("abc", true, '$', 0, 1, 64).unwrap();
        assert_eq!(
            hits.iter()
                .map(|hit| (hit.start, hit.end))
                .collect::<Vec<_>>(),
            vec![(1, 4), (5, 8)]
        );
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

    #[test]
    fn a_failed_overwrite_keeps_the_open_source_readable() {
        let (mut doc, path) = disk_doc("failed-save", &["a", "b"]);
        let original_permissions = std::fs::metadata(&path).unwrap().permissions();
        let mut readonly = original_permissions.clone();
        readonly.set_readonly(true);
        std::fs::set_permissions(&path, readonly).unwrap();
        assert!(doc.save(&path).is_err());
        std::fs::set_permissions(&path, original_permissions).unwrap();
        assert_eq!(doc.read(0, 2).unwrap(), vec!["a", "b"]);
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

    /// EOF seekによる末尾読みの実測。行数走査は開始しない。
    #[test]
    #[ignore]
    fn scale_check_reading_the_tail_without_line_count() {
        let path = r"C:\workspace\test-800mb.txt";
        if !std::path::Path::new(path).exists() {
            return;
        }
        let (mut doc, _scan) = Document::open(path).unwrap();
        let start = std::time::Instant::now();
        let tail = doc.read_tail(100).unwrap();
        println!("read tail: {:?} ({} lines)", start.elapsed(), tail.len());
        assert_eq!(tail.len(), 100);
    }

    /// 先頭1MBの同期読みと、残りの行数・間引き索引走査だけの実測。
    #[test]
    #[ignore]
    fn scale_check_counting_lines() {
        let path = r"C:\workspace\test-800mb.txt";
        if !std::path::Path::new(path).exists() {
            return;
        }
        let start = std::time::Instant::now();
        let (mut doc, scan) = Document::open(path).unwrap();
        let opened = start.elapsed();
        let start = std::time::Instant::now();
        if let Some(scan) = scan {
            scan.run().unwrap();
        }
        doc.confirm_scan();
        println!(
            "open: {opened:?}, scan: {:?}, lines: {}",
            start.elapsed(),
            doc.line_count()
        );
        assert_eq!(doc.line_count(), 16_000_001);
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
        let (mut doc, scan) = Document::open(path).unwrap();
        println!(
            "open (scanned {} lines): {:?}",
            doc.line_count(),
            start.elapsed()
        );
        let start = std::time::Instant::now();
        if let Some(scan) = scan {
            scan.run().unwrap();
        }
        doc.confirm_scan();
        println!(
            "scan (exact {} lines): {:?}",
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
        let missing = "planetext-not-present";
        let start = std::time::Instant::now();
        let _ = doc
            .scan_literal(missing, true, '$', 0, doc.line_count(), 64)
            .unwrap();
        println!("literal full scan: {:?}", start.elapsed());
        let start = std::time::Instant::now();
        let _ = doc
            .scan_literal("PLANETEXT-NOT-PRESENT", false, '$', 0, doc.line_count(), 64)
            .unwrap();
        println!("ASCII fold full scan: {:?}", start.elapsed());
        let mut snapshot = doc.search_snapshot().unwrap();
        let pattern = regex::Regex::new(missing).unwrap();
        let start = std::time::Instant::now();
        let searched = snapshot
            .search_candidates(
                SearchSpec {
                    pattern: &pattern,
                    literal: Some(missing),
                    case_sensitive: true,
                    marker: '$',
                    from: 0,
                    end: doc.line_count(),
                    after_col: None,
                },
                &|| false,
            )
            .unwrap();
        println!(
            "single native job ({} hits): {:?}",
            searched.hits.len(),
            start.elapsed()
        );
        let start = std::time::Instant::now();
        let estimate = doc
            .estimate_matches(&regex::Regex::new("fox").unwrap())
            .unwrap();
        println!("estimate ({estimate} matches): {:?}", start.elapsed());
    }
}
