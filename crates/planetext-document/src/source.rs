use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

/// 何行ごとに行頭のバイト位置を控えるか。1600 万行でも索引は数万個で済み、
/// どの行へも高々この行数だけ読み流せば届く。
pub(crate) const STRIDE: usize = 1024;

/// ファイルを読むときのひとかたまり。
pub(crate) const CHUNK: usize = 1 << 20;
pub(crate) const MAX_LINE_BYTES: usize = 20 << 20;

/// ファイルの文字エンコーディング。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum FileEncoding {
    Utf8,
    Utf8Bom,
    ShiftJis,
    EucJp,
    Iso2022Jp,
    Utf16Le,
    Utf16Be,
}

impl FileEncoding {
    pub(crate) fn label(&self) -> &'static str {
        match self {
            FileEncoding::Utf8 => "UTF-8",
            FileEncoding::Utf8Bom => "UTF-8 (BOM)",
            FileEncoding::ShiftJis => "Shift_JIS",
            FileEncoding::EucJp => "EUC-JP",
            FileEncoding::Iso2022Jp => "ISO-2022-JP",
            FileEncoding::Utf16Le => "UTF-16 LE",
            FileEncoding::Utf16Be => "UTF-16 BE",
        }
    }

    pub(crate) fn from_label(label: &str) -> Option<Self> {
        let normalized = label
            .trim()
            .to_ascii_uppercase()
            .replace(['-', '_', ' ', '(', ')'], "");
        match normalized.as_str() {
            "UTF8" => Some(FileEncoding::Utf8),
            "UTF8BOM" => Some(FileEncoding::Utf8Bom),
            "SHIFTJIS" | "SJIS" | "CP932" | "WINDOWS31J" => Some(FileEncoding::ShiftJis),
            "EUCJP" | "EUC" => Some(FileEncoding::EucJp),
            "ISO2022JP" | "JIS" => Some(FileEncoding::Iso2022Jp),
            "UTF16LE" | "UTF16" => Some(FileEncoding::Utf16Le),
            "UTF16BE" => Some(FileEncoding::Utf16Be),
            _ => None,
        }
    }

    pub(crate) fn encoding(&self) -> &'static encoding_rs::Encoding {
        match self {
            FileEncoding::Utf8 | FileEncoding::Utf8Bom => encoding_rs::UTF_8,
            FileEncoding::ShiftJis => encoding_rs::SHIFT_JIS,
            FileEncoding::EucJp => encoding_rs::EUC_JP,
            FileEncoding::Iso2022Jp => encoding_rs::ISO_2022_JP,
            FileEncoding::Utf16Le => encoding_rs::UTF_16LE,
            FileEncoding::Utf16Be => encoding_rs::UTF_16BE,
        }
    }

    pub(crate) fn decode_line(&self, bytes: &[u8]) -> String {
        match self {
            FileEncoding::Utf8 | FileEncoding::Utf8Bom => {
                String::from_utf8_lossy(bytes).into_owned()
            }
            _ => {
                let (cow, _, _) = self.encoding().decode(bytes);
                cow.into_owned()
            }
        }
    }

    pub(crate) fn encode_str(&self, text: &str) -> Vec<u8> {
        match self {
            FileEncoding::Utf8 | FileEncoding::Utf8Bom => text.as_bytes().to_vec(),
            FileEncoding::Utf16Le => text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
            FileEncoding::Utf16Be => text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
            _ => {
                let (cow, _, _) = self.encoding().encode(text);
                cow.into_owned()
            }
        }
    }

    pub(crate) fn unit_bytes(self) -> usize {
        match self {
            FileEncoding::Utf16Le | FileEncoding::Utf16Be => 2,
            _ => 1,
        }
    }

    fn bom_len(self, bytes: &[u8]) -> usize {
        match self {
            FileEncoding::Utf8 | FileEncoding::Utf8Bom if bytes.starts_with(b"\xEF\xBB\xBF") => 3,
            FileEncoding::Utf16Le if bytes.starts_with(b"\xFF\xFE") => 2,
            FileEncoding::Utf16Be if bytes.starts_with(b"\xFE\xFF") => 2,
            _ => 0,
        }
    }

    /// 先頭バイト列から文字コードを自動判別する。BOM の有無も返す。
    pub(crate) fn detect(bytes: &[u8]) -> (FileEncoding, bool) {
        if bytes.starts_with(b"\xEF\xBB\xBF") {
            return (FileEncoding::Utf8Bom, true);
        }
        if bytes.starts_with(b"\xFF\xFE") {
            return (FileEncoding::Utf16Le, true);
        }
        if bytes.starts_with(b"\xFE\xFF") {
            return (FileEncoding::Utf16Be, true);
        }
        // ISO-2022-JP のエスケープシーケンス判定
        if memchr::memmem::find(bytes, b"\x1b$B").is_some()
            || memchr::memmem::find(bytes, b"\x1b$@").is_some()
            || memchr::memmem::find(bytes, b"\x1b(J").is_some()
        {
            return (FileEncoding::Iso2022Jp, false);
        }
        // 完全な UTF-8 かどうか
        if std::str::from_utf8(bytes).is_ok() {
            return (FileEncoding::Utf8, false);
        }
        // chardetng による日本語文字コードの高精度推定
        let mut detector = chardetng::EncodingDetector::new();
        detector.feed(bytes, true);
        let guessed = detector.guess(Some(b"jp"), true);
        if guessed == encoding_rs::SHIFT_JIS {
            (FileEncoding::ShiftJis, false)
        } else if guessed == encoding_rs::EUC_JP {
            (FileEncoding::EucJp, false)
        } else if guessed == encoding_rs::ISO_2022_JP {
            (FileEncoding::Iso2022Jp, false)
        } else {
            (FileEncoding::Utf8, false)
        }
    }
}

/// 改行コード。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum LineEnding {
    CrLf,
    Lf,
    Cr,
}

impl LineEnding {
    pub(crate) fn label(&self) -> &'static str {
        match self {
            LineEnding::CrLf => "CRLF",
            LineEnding::Lf => "LF",
            LineEnding::Cr => "CR",
        }
    }

    pub(crate) fn from_label(label: &str) -> Option<Self> {
        match label.trim().to_ascii_uppercase().as_str() {
            "CRLF" => Some(LineEnding::CrLf),
            "LF" => Some(LineEnding::Lf),
            "CR" => Some(LineEnding::Cr),
            _ => None,
        }
    }

    pub(crate) fn as_bytes(&self) -> &'static [u8] {
        match self {
            LineEnding::CrLf => b"\r\n",
            LineEnding::Lf => b"\n",
            LineEnding::Cr => b"\r",
        }
    }

    /// バイト列から改行コードを判別する。
    pub(crate) fn detect_encoded(bytes: &[u8], encoding: FileEncoding) -> Self {
        let decoded;
        let bytes = if encoding.unit_bytes() == 1 {
            bytes
        } else {
            decoded = encoding.decode_line(bytes);
            decoded.as_bytes()
        };
        let crlf = memchr::memmem::find_iter(bytes, b"\r\n").count();
        let total_n = memchr::memchr_iter(b'\n', bytes).count();
        let lf_only = total_n.saturating_sub(crlf);
        let total_r = memchr::memchr_iter(b'\r', bytes).count();
        let cr_only = total_r.saturating_sub(crlf);
        if crlf >= lf_only && crlf >= cr_only && crlf > 0 {
            LineEnding::CrLf
        } else if lf_only >= cr_only && lf_only > 0 {
            LineEnding::Lf
        } else if cr_only > crlf && cr_only > 0 {
            LineEnding::Cr
        } else {
            #[cfg(windows)]
            {
                LineEnding::CrLf
            }
            #[cfg(not(windows))]
            {
                LineEnding::Lf
            }
        }
    }
}

/// チャンク内の改行を数え、STRIDE 行ごとの行頭バイト位置を `marks` へ足す。
/// `line` は通し行数、`offset` はチャンク先頭のファイル内バイト位置。
pub(crate) fn index_chunk(
    chunk: &[u8],
    offset: u64,
    line: &mut usize,
    marks: &mut Vec<u64>,
    delimiter: &[u8],
    unit_bytes: usize,
) {
    index_chunk_impl(chunk, offset, line, marks, delimiter, unit_bytes);
}

/// 戻り値は、位置列挙が必要だった1バイトブロック数。通常ビルドでは捨てるが、
/// テストでは改行密度に依存しない高速経路を構造的に確認する。
fn index_chunk_impl(
    chunk: &[u8],
    offset: u64,
    line: &mut usize,
    marks: &mut Vec<u64>,
    delimiter: &[u8],
    unit_bytes: usize,
) -> usize {
    if unit_bytes == 1 {
        const COUNT_BLOCK: usize = 64 * 1024;
        let mut enumerated = 0;
        for (block_index, block) in chunk.chunks(COUNT_BLOCK).enumerate() {
            let count = memchr::memchr_iter(delimiter[0], block).count();
            let next_line = *line + count;
            if *line / STRIDE != next_line / STRIDE {
                enumerated += 1;
                let block_offset = offset + (block_index * COUNT_BLOCK) as u64;
                for at in memchr::memchr_iter(delimiter[0], block) {
                    *line += 1;
                    if (*line).is_multiple_of(STRIDE) {
                        marks.push(block_offset + at as u64 + 1);
                    }
                }
            } else {
                *line = next_line;
            }
        }
        enumerated
    } else {
        for at in (0..chunk.len().saturating_sub(1)).step_by(unit_bytes) {
            if &chunk[at..at + unit_bytes] == delimiter {
                *line += 1;
                if (*line).is_multiple_of(STRIDE) {
                    marks.push(offset + at as u64 + unit_bytes as u64);
                }
            }
        }
        0
    }
}

/// 走査スレッドが更新する間引き索引と行数。
pub(crate) struct ScanIndex {
    pub(crate) state: Mutex<ScanState>,
}

pub(crate) struct ScanState {
    /// STRIDE 行ごとの行頭のバイト位置。先頭は必ず 0。
    pub(crate) marks: Vec<u64>,
    /// 走査済みの行数。完了後は総行数。
    pub(crate) lines: usize,
    /// 走査が終わったか。
    pub(crate) done: bool,
    /// 途中で UTF-8 違反が見つかった場合の遅延エラー。
    pub(crate) broken: Option<String>,
}

impl ScanIndex {
    /// `Ok(Some(lines))` は完了、`Ok(None)` は走査中、`Err` は遅延エラー。
    pub(crate) fn status(&self) -> Result<Option<usize>, String> {
        let index = self.state.lock().unwrap();
        match &index.broken {
            Some(error) => Err(error.clone()),
            None if index.done => Ok(Some(index.lines)),
            None => Ok(None),
        }
    }
}

/// 開いたファイル。行の中身はここから seek で読む。
pub(crate) struct Source {
    pub(crate) path: PathBuf,
    pub(crate) file: File,
    /// 行数と間引き索引はバックグラウンド走査スレッドと共有する。
    pub(crate) index: Arc<ScanIndex>,
    pub(crate) bytes: u64,
    pub(crate) content_offset: u64,
    /// バックグラウンド走査中に、同期走査で確定した最後の改行直後。
    /// この位置から EOF までが、Document が独立して所有する未走査範囲になる。
    pub(crate) pending_from: Option<u64>,
    pub(crate) ends_with_newline: bool,
    /// 開いたときの姿。外から書き換えられると seek 読みが壊れるので、
    /// 変わっていたら読む前に断る。
    pub(crate) modified: Option<SystemTime>,
    pub(crate) encoding: FileEncoding,
    pub(crate) line_ending: LineEnding,
}

pub(crate) struct BackgroundScan {
    reader: BufReader<File>,
    index: Arc<ScanIndex>,
    path: PathBuf,
    offset: u64,
    line: usize,
    delimiter: Vec<u8>,
    unit_bytes: usize,
}

impl BackgroundScan {
    pub(crate) fn run(mut self) -> Result<Option<usize>, String> {
        loop {
            if Arc::strong_count(&self.index) == 1 {
                return Ok(None);
            }
            let chunk = self
                .reader
                .fill_buf()
                .map_err(|e| format!("{} を読めませんでした: {e}", self.path.display()))?;
            if chunk.is_empty() {
                let mut index = self.index.state.lock().unwrap();
                index.lines = self.line + 1;
                index.done = true;
                return Ok(Some(index.lines));
            }
            let mut marks = Vec::new();
            index_chunk(
                chunk,
                self.offset,
                &mut self.line,
                &mut marks,
                &self.delimiter,
                self.unit_bytes,
            );
            let len = chunk.len();
            self.offset += len as u64;
            self.reader.consume(len);
            let mut index = self.index.state.lock().unwrap();
            index.marks.extend(marks);
            index.lines = self.line;
        }
    }
}

fn read_line_bounded<R: BufRead>(
    reader: &mut R,
    encoding: FileEncoding,
    delimiter: &[u8],
    buffer: &mut Vec<u8>,
) -> Result<usize, String> {
    buffer.clear();
    if encoding.unit_bytes() == 1 {
        let mut limited = reader.take((MAX_LINE_BYTES + 1) as u64);
        let read = limited
            .read_until(delimiter[0], buffer)
            .map_err(|e| e.to_string())?;
        if buffer.len() > MAX_LINE_BYTES {
            return Err("1行が20MBを超えるため表示できません".to_string());
        }
        return Ok(read);
    }
    let mut unit = [0; 2];
    loop {
        match reader.read_exact(&mut unit) {
            Ok(()) => {
                buffer.extend_from_slice(&unit);
                if buffer.len() > MAX_LINE_BYTES {
                    return Err("1行が20MBを超えるため表示できません".to_string());
                }
                if unit == delimiter {
                    return Ok(buffer.len());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Ok(buffer.len())
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

impl Source {
    pub(crate) fn delimiter(&self) -> Vec<u8> {
        let delimiter = match self.line_ending {
            LineEnding::Cr => "\r",
            _ => "\n",
        };
        match self.encoding {
            FileEncoding::Utf16Le => delimiter
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect(),
            FileEncoding::Utf16Be => delimiter
                .encode_utf16()
                .flat_map(u16::to_be_bytes)
                .collect(),
            _ => delimiter.as_bytes().to_vec(),
        }
    }

    pub(crate) fn open_with_encoding(
        path: &Path,
        specified_encoding: Option<FileEncoding>,
    ) -> Result<(Source, Option<BackgroundScan>), String> {
        let mut file =
            File::open(path).map_err(|e| format!("{} を開けませんでした: {e}", path.display()))?;
        let modified = file.metadata().ok().and_then(|meta| meta.modified().ok());
        let bytes = file.metadata().ok().map_or(0, |m| m.len());
        // try_clone はカーソルを共有し、読みの seek が走査の位置を壊すので、
        // 走査には独立したハンドルを開く。
        let scan_file =
            File::open(path).map_err(|e| format!("{} を読めませんでした: {e}", path.display()))?;
        let mut reader = BufReader::with_capacity(CHUNK, scan_file);
        let (encoding, initial_offset, line_ending) = {
            let chunk = reader
                .fill_buf()
                .map_err(|e| format!("{} を読めませんでした: {e}", path.display()))?;
            let encoding = specified_encoding.unwrap_or_else(|| FileEncoding::detect(chunk).0);
            let initial_offset = encoding.bom_len(chunk);
            let content = &chunk[initial_offset.min(chunk.len())..];
            (
                encoding,
                initial_offset,
                LineEnding::detect_encoded(content, encoding),
            )
        };
        let index = Arc::new(ScanIndex {
            state: Mutex::new(ScanState {
                marks: vec![initial_offset as u64],
                lines: 0,
                done: false,
                broken: None,
            }),
        });
        let delimiter_text = match line_ending {
            LineEnding::Cr => "\r",
            _ => "\n",
        };
        let delimiter = match encoding {
            FileEncoding::Utf16Le => delimiter_text
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
            FileEncoding::Utf16Be => delimiter_text
                .encode_utf16()
                .flat_map(u16::to_be_bytes)
                .collect::<Vec<_>>(),
            _ => delimiter_text.as_bytes().to_vec(),
        };
        let unit_bytes = encoding.unit_bytes();
        let mut marks = vec![initial_offset as u64];
        let mut line = 0;
        let mut offset = 0;
        let provisional_end;
        {
            let chunk_buf = reader
                .fill_buf()
                .map_err(|e| format!("{} を読めませんでした: {e}", path.display()))?;
            let chunk = if initial_offset > 0 && chunk_buf.len() >= initial_offset {
                &chunk_buf[initial_offset..]
            } else {
                chunk_buf
            };
            index_chunk(
                chunk,
                initial_offset as u64,
                &mut line,
                &mut marks,
                &delimiter,
                unit_bytes,
            );
            provisional_end = if unit_bytes == 1 {
                memchr::memrchr(delimiter[0], chunk)
                    .map_or(initial_offset, |at| initial_offset + at + 1)
            } else {
                (0..chunk.len().saturating_sub(1))
                    .step_by(unit_bytes)
                    .rev()
                    .find(|at| &chunk[*at..*at + unit_bytes] == delimiter.as_slice())
                    .map_or(initial_offset, |at| initial_offset + at + unit_bytes)
            };
            let len = chunk_buf.len();
            offset += len as u64;
            reader.consume(len);
        }
        // 最初のチャンクでファイル全体を読めたなら、行数はここで確定する。
        let done = offset >= bytes;
        {
            let mut state = index.state.lock().unwrap();
            state.marks = marks;
            state.lines = if done { line + 1 } else { line };
            state.done = done;
        }
        let ends_with_newline = if bytes >= initial_offset as u64 + delimiter.len() as u64 {
            let mut tail = vec![0; delimiter.len()];
            file.seek(SeekFrom::End(-(delimiter.len() as i64)))
                .and_then(|_| file.read_exact(&mut tail))
                .map_err(|e| format!("{} を読めませんでした: {e}", path.display()))?;
            tail == delimiter
        } else {
            false
        };
        let source = Source {
            path: path.to_path_buf(),
            file,
            index: index.clone(),
            bytes,
            content_offset: initial_offset as u64,
            pending_from: (!done).then_some(provisional_end as u64),
            ends_with_newline,
            modified,
            encoding,
            line_ending,
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
                delimiter,
                unit_bytes,
            })
        };
        Ok((source, scan))
    }

    /// 開いてから外で書き換えられていないか。壊れた seek 読みを返すより断る。
    pub(crate) fn check(&self) -> Result<(), String> {
        let meta = std::fs::metadata(&self.path)
            .map_err(|e| format!("{} を読めませんでした: {e}", self.path.display()))?;
        if meta.len() == self.bytes && meta.modified().ok() == self.modified {
            Ok(())
        } else {
            Err(format!(
                "{} が別の場所で変更されました。開き直してください",
                self.path.display()
            ))
        }
    }

    pub(crate) fn lines(&self) -> usize {
        self.index.state.lock().unwrap().lines
    }

    /// 総行数や間引き索引を待たず、EOFから後ろの行だけを読む。
    pub(crate) fn read_tail(&mut self, count: usize) -> Result<Vec<String>, String> {
        if count == 0 {
            return Ok(Vec::new());
        }
        if self.bytes == self.content_offset {
            return Ok(vec![String::new()]);
        }
        self.check()?;
        const TAIL_CHUNK: u64 = 64 << 10;
        const MAX_TAIL_BYTES: u64 = 8 << 20;
        let delimiter = self.delimiter();
        let mut at = self.bytes;
        let mut chunks = Vec::new();
        let mut newlines = 0;
        let mut read_bytes = 0;
        while at > self.content_offset && newlines < count && read_bytes < MAX_TAIL_BYTES {
            let mut from = at.saturating_sub(TAIL_CHUNK).max(self.content_offset);
            if self.encoding.unit_bytes() == 2 && !(from - self.content_offset).is_multiple_of(2) {
                from += 1;
            }
            let mut chunk = vec![0; (at - from) as usize];
            self.file
                .seek(SeekFrom::Start(from))
                .map_err(|e| format!("{} を読めませんでした: {e}", self.path.display()))?;
            self.file
                .read_exact(&mut chunk)
                .map_err(|e| format!("{} を読めませんでした: {e}", self.path.display()))?;
            newlines += if delimiter.len() == 1 {
                memchr::memchr_iter(delimiter[0], &chunk).count()
            } else {
                chunk
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .filter(|unit| *unit == delimiter.as_slice())
                    .count()
            };
            read_bytes += chunk.len() as u64;
            chunks.push(chunk);
            at = from;
        }
        if at > self.content_offset && newlines < count {
            return Err("末尾の行が8MBを超えるため表示できません".to_string());
        }
        chunks.reverse();
        let text = self
            .encoding
            .decode_line(&chunks.into_iter().flatten().collect::<Vec<_>>());
        let delimiter = match self.line_ending {
            LineEnding::Cr => '\r',
            _ => '\n',
        };
        let lines: Vec<&str> = text.split(delimiter).collect();
        let first = lines.len().saturating_sub(count);
        Ok(lines[first..]
            .iter()
            .map(|line| line.trim_end_matches(['\r', '\n']).to_string())
            .collect())
    }

    fn indexed_start(
        &mut self,
        from: usize,
        len: usize,
        skip: usize,
    ) -> Result<(usize, usize), String> {
        if skip < STRIDE {
            return Ok((from, skip));
        }
        let (before, mark) = {
            let index = self.index.state.lock().unwrap();
            let before = index
                .marks
                .partition_point(|offset| *offset <= from as u64)
                .saturating_sub(1);
            (before, index.marks[before] as usize)
        };
        let mut source_line = before * STRIDE;
        if mark < from {
            self.file
                .seek(SeekFrom::Start(mark as u64))
                .map_err(|e| e.to_string())?;
            let delimiter = self.delimiter();
            let mut left = from - mark;
            let mut buffer = vec![0; CHUNK.min(left.max(1))];
            while left > 0 {
                let take = left.min(buffer.len());
                self.file
                    .read_exact(&mut buffer[..take])
                    .map_err(|e| e.to_string())?;
                source_line += if delimiter.len() == 1 {
                    memchr::memchr_iter(delimiter[0], &buffer[..take]).count()
                } else {
                    buffer[..take]
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .filter(|unit| *unit == delimiter.as_slice())
                        .count()
                };
                left -= take;
            }
        }
        let target = source_line + skip;
        let indexed_line = target / STRIDE;
        let state = self.index.state.lock().unwrap();
        let available_idx = indexed_line.min(state.marks.len().saturating_sub(1));
        if let Some(&mark) = state.marks.get(available_idx) {
            let offset = mark as usize;
            if offset >= from && offset <= from.saturating_add(len) {
                return Ok((offset, target.saturating_sub(available_idx * STRIDE)));
            }
        }
        Ok((from, skip))
    }

    pub(crate) fn for_each_range_line(
        &mut self,
        from: usize,
        len: usize,
        skip: usize,
        take: usize,
        f: &mut dyn FnMut(usize, &str) -> bool,
    ) -> Result<bool, String> {
        self.check()?;
        let (start, mut remaining_skip) = self.indexed_start(from, len, skip)?;
        self.file
            .seek(SeekFrom::Start(start as u64))
            .map_err(|e| e.to_string())?;
        let available = from.saturating_add(len).saturating_sub(start);
        let limited = (&self.file).take(available as u64);
        let mut reader = BufReader::with_capacity(CHUNK.min(available.max(1)), limited);
        let delimiter = self.delimiter();
        let unit_bytes = self.encoding.unit_bytes();

        let mut ended_after_delimiter = false;
        // 目的行までの skip はバッファ確保を一切行わず SIMD で一気に飛ばす
        if remaining_skip > 0 {
            if unit_bytes == 1 {
                let delim_byte = delimiter[0];
                loop {
                    let chunk = reader.fill_buf().map_err(|e| e.to_string())?;
                    if chunk.is_empty() {
                        break;
                    }
                    let mut consumed = 0;
                    let mut found = false;
                    for pos in memchr::memchr_iter(delim_byte, chunk) {
                        remaining_skip -= 1;
                        if remaining_skip == 0 {
                            consumed = pos + 1;
                            found = true;
                            break;
                        }
                    }
                    if found {
                        reader.consume(consumed);
                        ended_after_delimiter = true;
                        break;
                    }
                    let chunk_len = chunk.len();
                    reader.consume(chunk_len);
                }
            } else {
                let delim_slice = delimiter.as_slice();
                loop {
                    let chunk = reader.fill_buf().map_err(|e| e.to_string())?;
                    if chunk.is_empty() {
                        break;
                    }
                    let mut consumed = 0;
                    let mut found = false;
                    let chunk_units = (chunk.len() / 2) * 2;
                    for i in (0..chunk_units).step_by(2) {
                        if &chunk[i..i + 2] == delim_slice {
                            remaining_skip -= 1;
                            if remaining_skip == 0 {
                                consumed = i + 2;
                                found = true;
                                break;
                            }
                        }
                    }
                    if found {
                        reader.consume(consumed);
                        ended_after_delimiter = true;
                        break;
                    }
                    let has_odd = chunk.len() > chunk_units;
                    reader.consume(chunk_units);
                    if has_odd {
                        break;
                    }
                }
            }
        }

        // 目的の take 行だけを bounded read で読み取って callback に渡す
        let mut buffer = Vec::new();
        for line in 0..take {
            let read = read_line_bounded(&mut reader, self.encoding, &delimiter, &mut buffer)?;
            if read == 0 {
                let indexed_empty_tail = start == from.saturating_add(len) && remaining_skip == 0;
                if remaining_skip == 0
                    && (len == 0
                        || ended_after_delimiter
                        || indexed_empty_tail
                        || self.ends_with_newline)
                    && !f(line, "")
                {
                    return Ok(false);
                }
                break;
            }
            ended_after_delimiter = buffer.ends_with(&delimiter);
            while buffer.ends_with(&self.encoding.encode_str("\n"))
                || buffer.ends_with(&self.encoding.encode_str("\r"))
            {
                buffer.truncate(buffer.len() - self.encoding.unit_bytes());
            }
            if !f(line, &self.encoding.decode_line(&buffer)) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// ピース内の指定行だけを検索できるよう、表示用の行長上限を使わずに
    /// 対象範囲の先頭・末尾を求める。
    pub(crate) fn byte_range_for_lines(
        &mut self,
        from: usize,
        len: usize,
        skip: usize,
        take: usize,
    ) -> Result<(usize, usize), String> {
        self.check()?;
        if take == 0 {
            return Ok((from, from));
        }
        let (start, remaining_skip) = self.indexed_start(from, len, skip)?;
        let range_end = from.saturating_add(len);
        self.file
            .seek(SeekFrom::Start(start as u64))
            .map_err(|e| e.to_string())?;
        let delimiter = self.delimiter();
        let unit = self.encoding.unit_bytes();
        let mut cursor = start;
        let mut skipped = 0;
        let mut selected_start = (remaining_skip == 0).then_some(start);
        let mut selected_lines = 0;
        let mut buffer = vec![0; CHUNK];
        while cursor < range_end {
            let read = (range_end - cursor).min(buffer.len());
            self.file
                .read_exact(&mut buffer[..read])
                .map_err(|e| e.to_string())?;
            let bytes = &buffer[..read];
            let delimiters: Box<dyn Iterator<Item = usize> + '_> = if unit == 1 {
                Box::new(memchr::memchr_iter(delimiter[0], bytes))
            } else {
                Box::new(
                    (0..bytes.len().saturating_sub(unit - 1))
                        .step_by(unit)
                        .filter(|at| &bytes[*at..*at + unit] == delimiter.as_slice()),
                )
            };
            for at in delimiters {
                let after = cursor + at + unit;
                if let Some(start_pos) = selected_start {
                    selected_lines += 1;
                    if selected_lines == take {
                        return Ok((start_pos, after));
                    }
                } else {
                    skipped += 1;
                    if skipped == remaining_skip {
                        selected_start = Some(after);
                    }
                }
            }
            cursor += read;
        }
        Ok((selected_start.unwrap_or(range_end), range_end))
    }

    pub(crate) fn read_byte_range(&mut self, from: usize, len: usize) -> Result<Vec<u8>, String> {
        self.check()?;
        self.file
            .seek(SeekFrom::Start(from as u64))
            .map_err(|e| e.to_string())?;
        let mut bytes = vec![0; len];
        self.file
            .read_exact(&mut bytes)
            .map_err(|e| e.to_string())?;
        Ok(bytes)
    }

    pub(crate) fn byte_offset_after_lines(
        &mut self,
        from: usize,
        len: usize,
        lines: usize,
    ) -> Result<usize, String> {
        self.check()?;
        if lines == 0 {
            return Ok(0);
        }
        let (start, mut remaining) = self.indexed_start(from, len, lines)?;
        if remaining == 0 {
            return Ok((start - from).min(len));
        }
        self.file
            .seek(SeekFrom::Start(start as u64))
            .map_err(|e| e.to_string())?;
        let available = from.saturating_add(len).saturating_sub(start);
        let limited = (&self.file).take(available as u64);
        let mut reader = BufReader::with_capacity(CHUNK.min(available.max(1)), limited);
        let delimiter = self.delimiter();
        let unit_bytes = self.encoding.unit_bytes();
        let mut offset = start - from;

        if unit_bytes == 1 {
            let delim_byte = delimiter[0];
            loop {
                let chunk = reader.fill_buf().map_err(|e| e.to_string())?;
                if chunk.is_empty() {
                    break;
                }
                let mut consumed = 0;
                let mut found = false;
                for pos in memchr::memchr_iter(delim_byte, chunk) {
                    remaining -= 1;
                    if remaining == 0 {
                        consumed = pos + 1;
                        found = true;
                        break;
                    }
                }
                if found {
                    offset += consumed;
                    reader.consume(consumed);
                    break;
                }
                let chunk_len = chunk.len();
                offset += chunk_len;
                reader.consume(chunk_len);
            }
        } else {
            let delim_slice = delimiter.as_slice();
            loop {
                let chunk = reader.fill_buf().map_err(|e| e.to_string())?;
                if chunk.is_empty() {
                    break;
                }
                let mut consumed = 0;
                let mut found = false;
                let chunk_units = (chunk.len() / 2) * 2;
                for i in (0..chunk_units).step_by(2) {
                    if &chunk[i..i + 2] == delim_slice {
                        remaining -= 1;
                        if remaining == 0 {
                            consumed = i + 2;
                            found = true;
                            break;
                        }
                    }
                }
                if found {
                    offset += consumed;
                    reader.consume(consumed);
                    break;
                }
                let has_odd = chunk.len() > chunk_units;
                offset += chunk_units;
                reader.consume(chunk_units);
                if has_odd {
                    break;
                }
            }
        }
        Ok(offset.min(len))
    }

    /// 検索スレッド専用の独立したファイルハンドルを持つ同じソース。
    pub(crate) fn search_copy(&self) -> Result<Source, String> {
        self.check()?;
        let file = File::open(&self.path)
            .map_err(|e| format!("{} を読めませんでした: {e}", self.path.display()))?;
        Ok(Source {
            path: self.path.clone(),
            file,
            index: self.index.clone(),
            bytes: self.bytes,
            content_offset: self.content_offset,
            pending_from: self.pending_from,
            ends_with_newline: self.ends_with_newline,
            modified: self.modified,
            encoding: self.encoding,
            line_ending: self.line_ending,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_byte_index_fast_path_matches_reference_across_offsets_and_densities() {
        fn reference(chunk: &[u8], offset: u64, initial_line: usize) -> (usize, Vec<u64>) {
            let mut line = initial_line;
            let mut marks = Vec::new();
            for at in memchr::memchr_iter(b'\n', chunk) {
                line += 1;
                if line.is_multiple_of(STRIDE) {
                    marks.push(offset + at as u64 + 1);
                }
            }
            (line, marks)
        }

        let cases = [
            (0, 0, vec![b'x'; 150_000]),
            (17, STRIDE - 2, b"a\nb\nc\nd\n".repeat(20_000)),
            (91_337, STRIDE * 3 + 11, b"\n".repeat(140_000)),
            (
                1_000_003,
                STRIDE - 1,
                (0..180_000)
                    .map(|at| if at % 997 == 0 { b'\n' } else { b'x' })
                    .collect(),
            ),
        ];
        for (offset, initial_line, chunk) in cases {
            let (expected_line, expected_marks) = reference(&chunk, offset, initial_line);
            let mut line = initial_line;
            let mut marks = Vec::new();
            index_chunk(&chunk, offset, &mut line, &mut marks, b"\n", 1);
            assert_eq!((line, marks), (expected_line, expected_marks));
        }

        let sparse = (0..256 * 1024)
            .map(|at| if at % 20_000 == 0 { b'\n' } else { b'x' })
            .collect::<Vec<_>>();
        let mut line = 0;
        let mut marks = Vec::new();
        let enumerated = index_chunk_impl(&sparse, 0, &mut line, &mut marks, b"\n", 1);
        assert_eq!(enumerated, 0);
        assert_eq!(line, memchr::memchr_iter(b'\n', &sparse).count());
    }

    #[test]
    fn far_range_start_uses_a_sparse_mark() {
        let path = std::env::temp_dir().join(format!(
            "planetext-source-{}-sparse-mark.txt",
            std::process::id()
        ));
        let text = (0..STRIDE * 2 + 8)
            .map(|line| format!("line-{line}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&path, text).unwrap();
        let (mut source, scan) = Source::open_with_encoding(&path, None).unwrap();
        if let Some(scan) = scan {
            scan.run().unwrap();
        }
        let marks = source.index.state.lock().unwrap().marks.clone();

        let (start, remaining) = source
            .indexed_start(
                source.content_offset as usize,
                source.bytes as usize - source.content_offset as usize,
                STRIDE + 3,
            )
            .unwrap();
        assert_eq!(start, marks[1] as usize);
        assert_eq!(remaining, 3);
        std::fs::remove_file(path).ok();
    }
}
