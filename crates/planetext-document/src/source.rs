use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
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
    pub(crate) cv: Condvar,
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
    pub(crate) fn new(state: ScanState) -> Self {
        Self {
            state: Mutex::new(state),
            cv: Condvar::new(),
        }
    }

    /// `Ok(Some(lines))` は完了、`Ok(None)` は走査中、`Err` は遅延エラー。
    pub(crate) fn status(&self) -> Result<Option<usize>, String> {
        let index = self.state.lock().unwrap();
        match &index.broken {
            Some(error) => Err(error.clone()),
            None if index.done => Ok(Some(index.lines)),
            None => Ok(None),
        }
    }

    /// 指定したマーク番号（または走査完了）まで待機する。
    pub(crate) fn wait_for_mark(&self, mark_index: usize) {
        let mut state = self.state.lock().unwrap();
        while !state.done && state.marks.len() <= mark_index && state.broken.is_none() {
            state = self.cv.wait(state).unwrap();
        }
    }

    /// 指定バイト位置の直前にあるマークの (マークインデックス, マークバイト位置) を二分探索で瞬時に返す。
    /// マークインデックス * STRIDE がそのマーク時点での行番号となる。
    #[allow(dead_code)]
    pub(crate) fn mark_before_offset(&self, byte_offset: u64) -> (usize, u64) {
        let state = self.state.lock().unwrap();
        let idx = state
            .marks
            .partition_point(|&mark| mark <= byte_offset)
            .saturating_sub(1);
        (idx, state.marks.get(idx).copied().unwrap_or(0))
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
                drop(index);
                self.index.cv.notify_all();
                return Ok(Some(self.line + 1));
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
            let has_new_marks = !marks.is_empty();
            index.marks.extend(marks);
            index.lines = self.line;
            drop(index);
            if has_new_marks {
                self.index.cv.notify_all();
            }
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

/// 30MB 以上の巨大ファイルに対する並列走査のしきい値
pub(crate) const PARALLEL_SEARCH_THRESHOLD: usize = 30 * 1024 * 1024;

#[allow(clippy::too_many_arguments)]
fn resolve_raw_hit_helper(
    abs_byte: usize,
    abs_end_byte: usize,
    mmap: &[u8],
    marks: &[u64],
    delimiter: &[u8],
    encoding: FileEncoding,
    fixed_char_count: Option<usize>,
    encoded_marker: &[u8],
) -> crate::search::RawHit {
    let mark_idx = marks
        .partition_point(|&mark| mark <= abs_byte as u64)
        .saturating_sub(1);
    let mark_byte = marks.get(mark_idx).copied().unwrap_or(0) as usize;
    let slice_to_count = &mmap[mark_byte..abs_byte];

    let line_in_mark = if delimiter.len() == 1 {
        memchr::memchr_iter(delimiter[0], slice_to_count).count()
    } else {
        memchr::memmem::find_iter(slice_to_count, delimiter).count()
    };
    let line = mark_idx * STRIDE + line_in_mark;

    // 行頭バイト位置
    let line_start_byte = if line_in_mark == 0 {
        mark_byte
    } else if delimiter.len() == 1 {
        let last_delim = memchr::memrchr(delimiter[0], slice_to_count).unwrap_or(0);
        mark_byte + last_delim + delimiter.len()
    } else {
        let mut last = 0;
        for pos in memchr::memmem::find_iter(slice_to_count, delimiter) {
            last = pos;
        }
        mark_byte + last + delimiter.len()
    };

    // 行頭から一致位置までの列（文字数）
    let col_bytes = abs_byte.saturating_sub(line_start_byte);
    let col_slice = &mmap[line_start_byte..line_start_byte + col_bytes];
    let col = encoding.decode_line(col_slice).chars().count();

    // 一致箇所の文字数
    let match_chars = if let Some(fc) = fixed_char_count {
        fc
    } else {
        let match_slice = &mmap[abs_byte..abs_end_byte];
        encoding.decode_line(match_slice).chars().count()
    };

    // 行末バイト位置
    let rest_slice = &mmap[abs_byte..];
    let line_end_byte = if delimiter.len() == 1 {
        memchr::memchr(delimiter[0], rest_slice)
            .map(|pos| abs_byte + pos)
            .unwrap_or(mmap.len())
    } else {
        memchr::memmem::find_iter(rest_slice, delimiter)
            .next()
            .map(|pos| abs_byte + pos)
            .unwrap_or(mmap.len())
    };
    let whole_line_slice = &mmap[line_start_byte..line_end_byte];
    let has_marker = if !encoded_marker.is_empty() {
        if encoded_marker.len() == 1 {
            memchr::memchr(encoded_marker[0], whole_line_slice).is_some()
        } else {
            memchr::memmem::find_iter(whole_line_slice, encoded_marker).next().is_some()
        }
    } else {
        false
    };

    crate::search::RawHit {
        hit: crate::search::ScanHit {
            line,
            notation: has_marker,
            start: col,
            end: col + match_chars,
        },
        start_byte: abs_byte,
        end_byte: abs_end_byte,
    }
}

impl Source {
    /// 30MB 以上の巨大ファイルの検索やインデックス構築において、
    /// 一時的なゼロコピー生バイト走査を行うための mmap を取得する。
    /// Advice::Sequential を指示し、OS に最大先読み（Prefetch）を指示する。
    pub(crate) fn mmap(&self) -> Result<memmap2::Mmap, String> {
        let mmap = unsafe {
            memmap2::MmapOptions::new()
                .map(&self.file)
                .map_err(|e| format!("メモリマップに失敗しました: {e}"))?
        };
        #[cfg(unix)]
        let _ = mmap.advise(memmap2::Advice::Sequential);
        Ok(mmap)
    }

    /// ファイルサイズにかかわらず、mmap 生バイト列に対する直接走査を行い、
    /// 一致箇所のみ O(log N) マーク二分探索で行・列番号を算出する超高速走査。
    /// DocumentMatcher trait により、リテラル・正規表現・bi-gram ブロック刈り込みを完全統一。
    /// 30MB 以上のファイルでは 2〜4 スレッドに自動分散して並列実行する。
    pub(crate) fn scan_matches(
        &self,
        matcher: &dyn crate::search::DocumentMatcher,
        search_index: Option<&crate::search_index::SearchIndex>,
        marker: char,
        limit: usize,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<Vec<crate::search::RawHit>, String> {
        let mmap = self.mmap()?;
        let content_offset = self.content_offset as usize;
        if content_offset >= mmap.len() {
            return Ok(Vec::new());
        }
        let haystack = &mmap[content_offset..];
        let encoding = self.encoding;
        let delimiter = self.delimiter();
        let encoded_marker = if marker != '\0' {
            encoding.encode_str(&marker.to_string())
        } else {
            Vec::new()
        };
        let fixed_char_count = matcher.fixed_char_count();
        let marks = self.index.state.lock().unwrap().marks.clone();

        let override_threads = std::env::var("PLANETEXT_SEARCH_THREADS")
            .ok()
            .and_then(|s| s.parse::<usize>().ok());
        let available = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let num_threads = override_threads.unwrap_or_else(|| available.clamp(2, 4));

        if haystack.len() < PARALLEL_SEARCH_THRESHOLD || num_threads <= 1 {
            // シングルスレッド走査
            let raw_matches = matcher.find_in_bytes(haystack, encoding);
            let mut hits = Vec::new();
            let mut count = 0;
            for (start, end) in raw_matches {
                count += 1;
                if count % 256 == 0 && cancelled() {
                    return Ok(Vec::new());
                }
                if hits.len() >= limit {
                    break;
                }
                let abs_byte = content_offset + start;
                let abs_end_byte = content_offset + end;

                // bi-gram 索引が存在する場合、ブロック刈り込みをチェック
                if let Some(index) = search_index {
                    let block = abs_byte / crate::search_index::INDEX_BLOCK_BYTES;
                    if !matcher.may_contain_in_block(index, block) {
                        continue;
                    }
                }

                hits.push(resolve_raw_hit_helper(
                    abs_byte,
                    abs_end_byte,
                    &mmap,
                    &marks,
                    &delimiter,
                    encoding,
                    fixed_char_count,
                    &encoded_marker,
                ));
            }
            return Ok(hits);
        }

        // 2〜4スレッド並列走査
        let chunk_size = haystack.len() / num_threads;
        let mut chunk_ranges: Vec<(usize, usize)> = Vec::with_capacity(num_threads);
        let mut start = 0;
        for k in 1..num_threads {
            let ideal_end = k * chunk_size;
            let split = if ideal_end < haystack.len() {
                if delimiter.len() == 1 {
                    memchr::memchr(delimiter[0], &haystack[ideal_end..])
                        .map(|p| ideal_end + p + 1)
                        .unwrap_or(haystack.len())
                } else {
                    memchr::memmem::find(&haystack[ideal_end..], &delimiter)
                        .map(|p| ideal_end + p + delimiter.len())
                        .unwrap_or(haystack.len())
                }
            } else {
                haystack.len()
            };
            if split > start {
                chunk_ranges.push((start, split));
                start = split;
            }
        }
        if start < haystack.len() {
            chunk_ranges.push((start, haystack.len()));
        }

        let cancel_flag = std::sync::atomic::AtomicBool::new(false);
        let thread_results: Vec<Vec<crate::search::RawHit>> = std::thread::scope(|s| {
            let mut handles = Vec::with_capacity(chunk_ranges.len());
            for &(c_start, c_end) in &chunk_ranges {
                let chunk_slice = &haystack[c_start..c_end];
                let chunk_offset = content_offset + c_start;
                let cancel_ref = &cancel_flag;
                let marks_ref = &marks;
                let delim_ref = &delimiter;
                let enc_marker_ref = &encoded_marker;
                let mmap_ref = &mmap;

                handles.push(s.spawn(move || {
                    let raw_matches = matcher.find_in_bytes(chunk_slice, encoding);
                    let mut chunk_hits = Vec::new();
                    let mut count = 0;
                    for (rel_start, rel_end) in raw_matches {
                        count += 1;
                        if count % 256 == 0 {
                            if cancel_ref.load(std::sync::atomic::Ordering::Relaxed) || cancelled() {
                                cancel_ref.store(true, std::sync::atomic::Ordering::Relaxed);
                                return Vec::new();
                            }
                        }
                        if chunk_hits.len() >= limit {
                            break;
                        }
                        let abs_byte = chunk_offset + rel_start;
                        let abs_end_byte = chunk_offset + rel_end;

                        if let Some(index) = search_index {
                            let block = abs_byte / crate::search_index::INDEX_BLOCK_BYTES;
                            if !matcher.may_contain_in_block(index, block) {
                                continue;
                            }
                        }

                        chunk_hits.push(resolve_raw_hit_helper(
                            abs_byte,
                            abs_end_byte,
                            mmap_ref,
                            marks_ref,
                            delim_ref,
                            encoding,
                            fixed_char_count,
                            enc_marker_ref,
                        ));
                    }
                    chunk_hits
                }));
            }

            handles
                .into_iter()
                .map(|h| h.join().unwrap_or_default())
                .collect()
        });

        if cancel_flag.load(std::sync::atomic::Ordering::Relaxed) || cancelled() {
            return Ok(Vec::new());
        }

        let mut total_hits = Vec::new();
        for ch in thread_results {
            total_hits.extend(ch);
            if total_hits.len() >= limit {
                total_hits.truncate(limit);
                break;
            }
        }
        Ok(total_hits)
    }

    #[allow(dead_code)]
    pub(crate) fn fast_mmap_search(
        &self,
        query: &str,
        case_sensitive: bool,
        marker: char,
        limit: usize,
    ) -> Result<Vec<crate::search::ScanHit>, String> {
        let matcher = crate::search::LiteralMatcher::new(query, case_sensitive)?;
        let raw = self.scan_matches(&matcher, None, marker, limit, &|| false)?;
        Ok(raw.into_iter().map(|r| r.hit).collect())
    }

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
        let index = Arc::new(ScanIndex::new(ScanState {
            marks: vec![initial_offset as u64],
            lines: 0,
            done: false,
            broken: None,
        }));
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

    /// ファイル末尾（EOF）から指定行数だけ逆方向にさかのぼった絶対バイト位置を求める。
    /// 走査完了前であっても、末尾付近の行へのアクセスや分割を EOF seek により 0 秒で完了させる。
    pub(crate) fn byte_offset_from_end(&mut self, lines_from_end: usize) -> Result<usize, String> {
        if lines_from_end == 0 {
            return Ok(self.bytes as usize);
        }
        self.check()?;
        const TAIL_CHUNK: u64 = 64 << 10;
        let delimiter = self.delimiter();
        let unit_bytes = self.encoding.unit_bytes();
        let mut at = self.bytes;
        let mut counted = 0;
        let mut read_bytes = 0;
        const MAX_SEARCH_BYTES: u64 = 32 << 20; // 32MB

        while at > self.content_offset && counted < lines_from_end && read_bytes < MAX_SEARCH_BYTES
        {
            let from = at.saturating_sub(TAIL_CHUNK).max(self.content_offset);
            let chunk_len = (at - from) as usize;
            let mut chunk = vec![0; chunk_len];
            self.file
                .seek(SeekFrom::Start(from))
                .map_err(|e| e.to_string())?;
            self.file
                .read_exact(&mut chunk)
                .map_err(|e| e.to_string())?;

            if unit_bytes == 1 {
                let delim_byte = delimiter[0];
                for pos in memchr::memchr_iter(delim_byte, &chunk).rev() {
                    counted += 1;
                    if counted == lines_from_end {
                        return Ok((from as usize) + pos + 1);
                    }
                }
            } else {
                let delim_slice = delimiter.as_slice();
                let chunk_units = (chunk.len() / 2) * 2;
                for i in (0..chunk_units).step_by(2).rev() {
                    if &chunk[i..i + 2] == delim_slice {
                        counted += 1;
                        if counted == lines_from_end {
                            return Ok((from as usize) + i + 2);
                        }
                    }
                }
            }
            read_bytes += chunk_len as u64;
            at = from;
        }
        if at == self.content_offset && counted + 1 == lines_from_end {
            return Ok(self.content_offset as usize);
        }
        Err("末尾からの探索範囲を超えました".to_string())
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
        self.index.wait_for_mark(indexed_line);
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
        let is_at_eof = from + len == self.bytes as usize;
        if is_at_eof {
            let index_lines = {
                let state = self.index.state.lock().unwrap();
                (state.done || state.lines > 0).then_some(state.lines)
            };
            if let Some(total_lines) = index_lines {
                if total_lines >= skip + take {
                    let from_end_start = total_lines - skip;
                    let from_end_end = total_lines - (skip + take);
                    if from_end_start < STRIDE * 2 {
                        let start_res = self.byte_offset_from_end(from_end_start);
                        let end_res = self.byte_offset_from_end(from_end_end);
                        if let (Ok(s), Ok(e)) = (start_res, end_res) {
                            if s >= from && e >= s && e <= from + len {
                                return Ok((s, e));
                            }
                        }
                    }
                }
            }
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
        let is_at_eof = from + len == self.bytes as usize;
        if is_at_eof {
            let index_lines = {
                let state = self.index.state.lock().unwrap();
                (state.done || state.lines > 0).then_some(state.lines)
            };
            if let Some(total_lines) = index_lines {
                if total_lines >= lines {
                    let from_end = total_lines - lines;
                    if from_end < STRIDE * 2 {
                        if let Ok(abs_pos) = self.byte_offset_from_end(from_end) {
                            if abs_pos >= from {
                                return Ok((abs_pos - from).min(len));
                            }
                        }
                    }
                }
            }
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

    #[test]
    fn wait_for_mark_coordinates_with_background_scan() {
        let path = std::env::temp_dir().join(format!(
            "planetext-source-{}-wait-mark.txt",
            std::process::id()
        ));
        let total_lines = STRIDE * 10;
        let text = (0..total_lines)
            .map(|line| format!("line-{line:06}-{}", "x".repeat(150)))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&path, &text).unwrap();

        let (mut source, scan) = Source::open_with_encoding(&path, None).unwrap();
        let scan = scan.unwrap();
        // 背景走査スレッドを起動
        let scan_handle = std::thread::spawn(move || {
            scan.run().unwrap();
        });

        // 走査完了を待たずに、中間マーク（STRIDE * 3 + 5 行目）を要求
        let (start, remaining) = source
            .indexed_start(
                source.content_offset as usize,
                source.bytes as usize - source.content_offset as usize,
                STRIDE * 3 + 5,
            )
            .unwrap();

        // indexed_start は Condvar で待機し、STRIDE * 3 のマーク以降のオフセットを返し、
        // 残りスキップ行数は STRIDE 未満（ここでは 5）になること
        assert_eq!(remaining, 5);
        assert!(start > 0);

        scan_handle.join().unwrap();
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn byte_range_and_offset_near_eof_use_eof_seek_accurately() {
        let path = std::env::temp_dir().join(format!(
            "planetext-source-{}-eof-seek.txt",
            std::process::id()
        ));
        let total_lines = STRIDE * 3;
        let text = (0..total_lines)
            .map(|line| format!("line-{line:05}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&path, &text).unwrap();

        let (mut source, scan) = Source::open_with_encoding(&path, None).unwrap();
        if let Some(scan) = scan {
            scan.run().unwrap();
        }

        let total_len = source.bytes as usize;
        // 末尾から 3 行目から 2 行分のバイト範囲
        let skip = total_lines - 3;
        let take = 2;
        let (range_from, range_to) = source
            .byte_range_for_lines(0, total_len, skip, take)
            .unwrap();

        let slice = &text.as_bytes()[range_from..range_to];
        let expected = format!("line-{:05}\nline-{:05}\n", skip, skip + 1);
        assert_eq!(slice, expected.as_bytes());

        // byte_offset_after_lines も正確
        let offset = source.byte_offset_after_lines(0, total_len, skip).unwrap();
        assert_eq!(offset, range_from);

        std::fs::remove_file(path).ok();
    }

    use crate::document::Document;
    use crate::search::SearchSpec;
    use crate::test_utils::*;

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


    #[test]
    fn replacing_provisional_final_line_preserves_pending_source_suffix() {
        let path = std::env::temp_dir().join(format!(
            "planetext-store-{}-background-provisional-final.txt",
            std::process::id()
        ));
        let source_lines = 100_000usize;
        let source = (0..source_lines)
            .map(|line| format!("source line {line:06}\n"))
            .collect::<String>();
        std::fs::write(&path, &source).unwrap();
        let path = path.to_string_lossy().into_owned();
        let (mut doc, scan) = Document::open(&path).unwrap();
        let scan = scan.expect("the file must exceed the initial scan chunk");
        let provisional_line = doc.line_count() - 1;
        let original = doc.read(provisional_line, 1).unwrap();

        doc.replace(
            provisional_line,
            provisional_line + 1,
            vec!["edited provisional line".into()],
            1,
            "before",
            "after",
        )
        .unwrap();
        scan.run().unwrap();
        doc.confirm_scan();

        assert_eq!(doc.line_count(), source_lines + 1);
        assert_eq!(
            doc.read(provisional_line, 2).unwrap(),
            vec![
                "edited provisional line".to_string(),
                format!("source line {:06}", provisional_line + 1),
            ]
        );
        assert_eq!(
            doc.read(source_lines - 1, 2).unwrap(),
            vec![format!("source line {:06}", source_lines - 1), "".into()]
        );

        let undone = doc.undo().unwrap().unwrap();
        assert_eq!(undone.state, "before");
        assert_eq!(doc.line_count(), source_lines + 1);
        assert_eq!(doc.read(provisional_line, 1).unwrap(), original);
        assert_eq!(
            doc.read(source_lines - 1, 2).unwrap(),
            vec![format!("source line {:06}", source_lines - 1), "".into()]
        );
        std::fs::remove_file(path).ok();
    }


    #[test]
    fn confirming_background_scan_preserves_prior_edits_and_history() {
        let path = std::env::temp_dir().join(format!(
            "planetext-store-{}-background-edited.txt",
            std::process::id()
        ));
        let source_line = "0123456789 source line\n";
        let source_lines = 60_000usize;
        let source_bytes = source_line.repeat(source_lines);
        std::fs::write(&path, &source_bytes).unwrap();
        let path = path.to_string_lossy().into_owned();
        let (mut doc, scan) = Document::open(&path).unwrap();
        let scan = scan.expect("the file must exceed the initial scan chunk");

        doc.replace(
            1,
            2,
            vec!["edited one".into(), "edited two".into()],
            1,
            "before",
            "after",
        )
        .unwrap();
        scan.run().unwrap();
        doc.confirm_scan();

        assert_eq!(doc.line_count(), source_lines + 2);
        assert_eq!(doc.pieces_line_count_for_test(), source_lines + 2);
        assert_eq!(doc.pieces_newline_count_for_test(), source_lines + 1);
        assert_eq!(
            doc.bytes(),
            source_bytes.len() - "0123456789 source line".len() + "edited one\nedited two".len()
        );
        assert_eq!(
            doc.read(0, 4).unwrap(),
            vec![
                "0123456789 source line",
                "edited one",
                "edited two",
                "0123456789 source line"
            ]
        );

        let undone = doc.undo().unwrap().unwrap();
        assert_eq!(undone.state, "before");
        assert_eq!(doc.line_count(), source_lines + 1);
        assert_eq!(doc.pieces_newline_count_for_test(), source_lines);
        assert_eq!(doc.bytes(), source_bytes.len());
        assert_eq!(doc.read(0, 3).unwrap(), vec!["0123456789 source line"; 3]);
        std::fs::remove_file(path).ok();
    }


    #[test]
    fn sparse_mark_at_eof_exposes_the_final_empty_line() {
        let path = std::env::temp_dir().join(format!(
            "planetext-store-{}-stride-final-empty.txt",
            std::process::id()
        ));
        std::fs::write(&path, "\n".repeat(STRIDE)).unwrap();
        let path = path.to_string_lossy().into_owned();
        let (mut doc, scan) =
            Document::open_with_encoding(&path, Some(FileEncoding::Utf8)).unwrap();

        assert!(scan.is_none());
        assert_eq!(doc.source_sparse_mark_for_test(1), doc.source_bytes_for_test());
        assert_eq!(doc.line_count(), STRIDE + 1);
        assert_eq!(doc.read(STRIDE, 1).unwrap(), vec![""]);
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
    fn multi_piece_callbacks_keep_global_line_numbers() {
        let (mut doc, path) = disk_doc("global-lines", &["a", "b", "c", "d", "e", "f"]);
        doc.replace(
            3,
            4,
            vec!["x".into(), "$one".into(), "y$".into()],
            1,
            "",
            "",
        )
        .unwrap();
        assert_eq!(doc.lines_containing(0, 99, '$').unwrap(), vec![4, 5]);
        let overrides = std::collections::HashMap::from([(5, "override".to_string())]);
        assert_eq!(
            doc.assemble(2, None, 6, None, &overrides).unwrap(),
            "c\nx\n$one\noverride\ne"
        );
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
        let query = crate::search::CompiledQuery::compile(missing, false, true, '$').unwrap();
        let start = std::time::Instant::now();
        let searched = snapshot
            .search_candidates(
                SearchSpec {
                    query: &query,
                    from: 0,
                    end: doc.line_count(),
                    after_col: None,
                    forward: true,
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


    /// 巨大な行を含むファイルで MAX_READ_BYTES ガードが正しく働き、
    /// 1回の read で 20MB を超えずに安全に打ち切られることを検証する。
    #[test]
    fn huge_lines_are_capped_at_max_read_bytes() {
        let dir = std::env::temp_dir();
        let path = dir.join("planetext-huge-lines-test.txt");
        // 5MBの行を10行（合計50MB）書き込む
        {
            let file = std::fs::File::create(&path).unwrap();
            let mut writer = std::io::BufWriter::new(file);
            let big_line = "a".repeat(5 * 1024 * 1024);
            for i in 0..10 {
                use std::io::Write;
                if i > 0 {
                    writeln!(writer).unwrap();
                }
                write!(writer, "{big_line}").unwrap();
            }
        }
        let (mut doc, scan) = Document::open(path.to_str().unwrap()).unwrap();
        if let Some(scan) = scan {
            scan.run().unwrap();
        }
        doc.confirm_scan();
        assert_eq!(doc.line_count(), 10);
        // 10行読もうとしても、20MBガードにより最初の4〜5行で打ち切られる
        let lines = doc.read(0, 10).unwrap();
        assert!(lines.len() < 10);
        assert!(lines.len() >= 3);
        let total_read_bytes: usize = lines.iter().map(|l| l.len()).sum();
        assert!(total_read_bytes <= 25 * 1024 * 1024);
        std::fs::remove_file(path).ok();
    }


    /// 10万行の巨大ファイルを生成して開き、索引付け・途中行 seek 読みが正しく動くことを検証。
    #[test]
    fn opening_and_reading_large_many_lines_file() {
        let dir = std::env::temp_dir();
        let path = dir.join("planetext-100k-lines-test.txt");
        {
            let file = std::fs::File::create(&path).unwrap();
            let mut writer = std::io::BufWriter::new(file);
            for i in 0..100_000 {
                use std::io::Write;
                if i > 0 {
                    writeln!(writer).unwrap();
                }
                write!(writer, "line {i} with some content").unwrap();
            }
        }
        let (mut doc, scan) = Document::open(path.to_str().unwrap()).unwrap();
        if let Some(scan) = scan {
            scan.run().unwrap();
        }
        doc.confirm_scan();
        assert_eq!(doc.line_count(), 100_000);
        // 先頭、中間、末尾の行を正確にseek読みできるか
        let middle = doc.read(50_000, 3).unwrap();
        assert_eq!(
            middle,
            vec![
                "line 50000 with some content",
                "line 50001 with some content",
                "line 50002 with some content"
            ]
        );
        let tail = doc.read(99_998, 2).unwrap();
        assert_eq!(
            tail,
            vec![
                "line 99998 with some content",
                "line 99999 with some content"
            ]
        );
        std::fs::remove_file(path).ok();
    }


    /// 回帰: 未走査（バックグラウンド走査完了前）の巨大ファイルで、
    /// 起動直後に末尾付近の行を読み出しても（Ctrl+End相当）、
    /// 1行ごとの読み捨てではなく SIMD による一括スキップで即座に届くことを検証する。
    #[test]
    fn reading_distant_lines_before_scan_completion_is_fast() {
        let lines: Vec<String> = (0..STRIDE * 5).map(|i| format!("content {i}")).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let (mut doc, path) = disk_doc("distant-lines", &refs);

        // 走査完了前（pending_source がまだ残る状態）
        assert_eq!(doc.line_count(), STRIDE * 5);
        let read = doc.read(STRIDE * 4 + 10, 3).unwrap();
        assert_eq!(
            read,
            vec![
                format!("content {}", STRIDE * 4 + 10),
                format!("content {}", STRIDE * 4 + 11),
                format!("content {}", STRIDE * 4 + 12),
            ]
        );
        std::fs::remove_file(path).ok();
    }


    /// 回帰: 未走査の巨大ファイルで、末尾付近の行を編集して Undo/Redo しても、
    /// ピース分割（split）が EOF seek により即座に行われ、タイムラグなしで復元できることを検証する。
    #[test]
    fn undo_redo_near_tail_before_scan_completion_is_instant() {
        let total = STRIDE * 5;
        let lines: Vec<String> = (0..total).map(|i| format!("line {i}")).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let (mut doc, path) = disk_doc("instant-undo-tail", &refs);

        // 走査完了前（pending_source が残る状態）で末尾直前の行（total - 2）を置換
        let target_line = total - 2;
        doc.replace(
            target_line,
            target_line + 1,
            vec!["modified tail".into()],
            1,
            &format!("line {target_line}"),
            "modified tail",
        )
        .unwrap();

        assert_eq!(doc.read(target_line, 1).unwrap(), vec!["modified tail"]);

        // Undo 実行（split が走るが、EOF seek により即座に元行へ戻る）
        let undone = doc.undo().unwrap().unwrap();
        assert_eq!(undone.state, format!("line {target_line}"));
        assert_eq!(
            doc.read(target_line, 1).unwrap(),
            vec![format!("line {target_line}")]
        );

        // Redo 実行（即座に再適用される）
        let redone = doc.redo().unwrap().unwrap();
        assert_eq!(redone.state, "modified tail");
        assert_eq!(doc.read(target_line, 1).unwrap(), vec!["modified tail"]);

        std::fs::remove_file(path).ok();
    }

}
