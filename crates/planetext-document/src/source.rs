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
            _ => {
                let (cow, _, _) = self.encoding().encode(text);
                cow.into_owned()
            }
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
    pub(crate) fn detect(bytes: &[u8]) -> Self {
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
    delimiter: u8,
) {
    // memchrのcountはSIMD実装を持つ。4KBごとに改行数だけ先に数え、STRIDEの
    // 境界を含む小区間だけ位置を列挙することで、debugでも全改行をRust側で
    // 1件ずつ反復しない。
    const BLOCK: usize = 4 << 10;
    for (block_index, block) in chunk.chunks(BLOCK).enumerate() {
        let count = memchr::memchr_iter(delimiter, block).count();
        let next_mark = (*line / STRIDE + 1) * STRIDE;
        if *line + count < next_mark {
            *line += count;
            continue;
        }
        for at in memchr::memchr_iter(delimiter, block) {
            *line += 1;
            if (*line).is_multiple_of(STRIDE) {
                marks.push(offset + (block_index * BLOCK + at) as u64 + 1);
            }
        }
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
    delimiter: u8,
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
                self.delimiter,
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

impl Source {
    pub(crate) fn delimiter(&self) -> u8 {
        match self.line_ending {
            LineEnding::Cr => b'\r',
            _ => b'\n',
        }
    }

    pub(crate) fn open_with_encoding(
        path: &Path,
        specified_encoding: Option<FileEncoding>,
    ) -> Result<(Source, Option<BackgroundScan>), String> {
        let file =
            File::open(path).map_err(|e| format!("{} を開けませんでした: {e}", path.display()))?;
        let modified = file.metadata().ok().and_then(|meta| meta.modified().ok());
        let bytes = file.metadata().ok().map_or(0, |m| m.len());
        // try_clone はカーソルを共有し、読みの seek が走査の位置を壊すので、
        // 走査には独立したハンドルを開く。
        let scan_file =
            File::open(path).map_err(|e| format!("{} を読めませんでした: {e}", path.display()))?;
        let mut reader = BufReader::with_capacity(CHUNK, scan_file);
        let (encoding, has_bom, line_ending) = {
            let chunk = reader
                .fill_buf()
                .map_err(|e| format!("{} を読めませんでした: {e}", path.display()))?;
            let (detected_enc, has_bom) = match specified_encoding {
                Some(enc) => (
                    enc,
                    enc == FileEncoding::Utf8Bom && chunk.starts_with(b"\xEF\xBB\xBF"),
                ),
                None => FileEncoding::detect(chunk),
            };
            (detected_enc, has_bom, LineEnding::detect(chunk))
        };
        let initial_offset =
            if has_bom && (encoding == FileEncoding::Utf8Bom || encoding == FileEncoding::Utf8) {
                3
            } else {
                0
            };
        let index = Arc::new(ScanIndex {
            state: Mutex::new(ScanState {
                marks: vec![initial_offset],
                lines: 0,
                done: false,
                broken: None,
            }),
        });
        let delimiter = match line_ending {
            LineEnding::Cr => b'\r',
            _ => b'\n',
        };
        let mut marks = vec![initial_offset];
        let mut line = 0;
        let mut offset = 0;
        {
            let chunk_buf = reader
                .fill_buf()
                .map_err(|e| format!("{} を読めませんでした: {e}", path.display()))?;
            let chunk = if initial_offset > 0 && chunk_buf.len() >= initial_offset as usize {
                &chunk_buf[initial_offset as usize..]
            } else {
                chunk_buf
            };
            index_chunk(chunk, initial_offset, &mut line, &mut marks, delimiter);
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
        let source = Source {
            path: path.to_path_buf(),
            file,
            index: index.clone(),
            bytes,
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
        if count == 0 || self.bytes == 0 {
            return Ok(Vec::new());
        }
        self.check()?;
        const TAIL_CHUNK: u64 = 64 << 10;
        const MAX_TAIL_BYTES: u64 = 8 << 20;
        let delimiter = self.delimiter();
        let mut at = self.bytes;
        let mut chunks = Vec::new();
        let mut newlines = 0;
        let mut read_bytes = 0;
        while at > 0 && newlines < count && read_bytes < MAX_TAIL_BYTES {
            let from = at.saturating_sub(TAIL_CHUNK);
            let mut chunk = vec![0; (at - from) as usize];
            self.file
                .seek(SeekFrom::Start(from))
                .map_err(|e| format!("{} を読めませんでした: {e}", self.path.display()))?;
            self.file
                .read_exact(&mut chunk)
                .map_err(|e| format!("{} を読めませんでした: {e}", self.path.display()))?;
            newlines += memchr::memchr_iter(delimiter, &chunk).count();
            read_bytes += chunk.len() as u64;
            chunks.push(chunk);
            at = from;
        }
        if at > 0 && newlines < count {
            return Err("末尾の行が8MBを超えるため表示できません".to_string());
        }
        chunks.reverse();
        let bytes: Vec<u8> = chunks.into_iter().flatten().collect();
        let lines: Vec<&[u8]> = bytes.split(|byte| *byte == delimiter).collect();
        let first = lines.len().saturating_sub(count);
        lines[first..]
            .iter()
            .map(|raw_line| {
                let mut line = *raw_line;
                while line.last() == Some(&b'\r') || line.last() == Some(&b'\n') {
                    line = &line[..line.len() - 1];
                }
                Ok(self.encoding.decode_line(line))
            })
            .collect()
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
            modified: self.modified,
            encoding: self.encoding,
            line_ending: self.line_ending,
        })
    }

    /// ディスク上の行 `from` から `count` 行に `f` を呼ぶ。`f` が `false` を
    /// 返したら打ち切る。最寄りの索引へ seek し、そこから読み流す。
    pub(crate) fn each_line(
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
        let delimiter = self.delimiter();
        // 遅延索引が目的行へまだ届いていなければ、最後にある印から正しく読む。
        for _ in 0..from - mark_index * STRIDE {
            buffer.clear();
            reader.read_until(delimiter, &mut buffer).map_err(broken)?;
        }
        let to = (from + count).min(lines);
        for line in from..to {
            buffer.clear();
            reader.read_until(delimiter, &mut buffer).map_err(broken)?;
            while buffer.last() == Some(&b'\n') || buffer.last() == Some(&b'\r') {
                buffer.pop();
            }
            let text = self.encoding.decode_line(&buffer);
            if !f(line, &text) {
                return Ok(());
            }
        }
        Ok(())
    }
}
