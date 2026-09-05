use crate::document::Document;
use crate::source::{FileEncoding, CHUNK};

/// 通常文字列の一致バイト位置。ASCIIの大小無視は先頭バイト候補だけを調べ、
/// 候補ごとにASCII case-foldで比較する。結果はregexと同じ非重複一致。
pub(crate) fn literal_positions(
    haystack: &[u8],
    needle: &[u8],
    case_sensitive: bool,
) -> Vec<usize> {
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
    let mut next = 0;
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
#[derive(Clone, Debug)]
pub(crate) struct SearchHitCache {
    pub(crate) pattern: String,
    pub(crate) literal: Option<String>,
    pub(crate) case_sensitive: bool,
    pub(crate) revision: u64,
    pub(crate) hits: Vec<ScanHit>,
    pub(crate) fully_scanned: bool,
}


#[allow(dead_code)]
#[derive(Clone, Copy)]
pub(crate) struct RawScanHit {
    pub(crate) line: usize,
    pub(crate) notation: bool,
    pub(crate) line_start: usize,
    pub(crate) start: usize,
}

#[allow(dead_code)]
fn character_columns(
    encoding: FileEncoding,
    positions: &[usize],
    mut read: impl FnMut(usize, usize) -> Result<Vec<u8>, String>,
) -> Result<Vec<usize>, String> {
    let mut decoder = encoding.encoding().new_decoder_without_bom_handling();
    let mut byte = 0;
    let mut characters = 0;
    let output_capacity = positions.last().copied().unwrap_or(0).min(CHUNK) * 3;
    let mut output = String::with_capacity(output_capacity);
    let mut columns = Vec::with_capacity(positions.len());
    for &position in positions {
        while byte < position {
            let take = (position - byte).min(CHUNK);
            let input = read(byte, take)?;
            if input.len() != take {
                return Err("検索中に文書ストアの読み取り範囲が変わりました".to_string());
            }
            let mut consumed = 0;
            while consumed < input.len() {
                output.clear();
                let (result, read, _) =
                    decoder.decode_to_string(&input[consumed..], &mut output, false);
                consumed += read;
                characters += output.chars().count();
                if matches!(result, encoding_rs::CoderResult::InputEmpty) {
                    break;
                }
            }
            byte += take;
        }
        columns.push(characters);
    }
    Ok(columns)
}

#[allow(dead_code)]
pub(crate) fn convert_raw_hits(
    raw: Vec<RawScanHit>,
    encoding: FileEncoding,
    line_base: usize,
    query_characters: usize,
    mut read: impl FnMut(usize, usize) -> Result<Vec<u8>, String>,
) -> Result<Vec<ScanHit>, String> {
    let mut converted = Vec::with_capacity(raw.len());
    let mut at = 0;
    while at < raw.len() {
        let hit = raw[at];
        if hit.notation {
            converted.push(ScanHit {
                line: line_base + hit.line,
                notation: true,
                start: 0,
                end: 0,
            });
            at += 1;
            continue;
        }
        let mut end = at + 1;
        while end < raw.len() && raw[end].line == hit.line && !raw[end].notation {
            end += 1;
        }
        let positions: Vec<usize> = raw[at..end]
            .iter()
            .map(|found| found.start - found.line_start)
            .collect();
        let columns = character_columns(encoding, &positions, |from, len| {
            read(hit.line_start + from, len)
        })?;
        for (found, start) in raw[at..end].iter().zip(columns) {
            converted.push(ScanHit {
                line: line_base + found.line,
                notation: false,
                start,
                end: start + query_characters,
            });
        }
        at = end;
    }
    Ok(converted)
}

#[allow(dead_code)]
fn aligned_positions(
    haystack: &[u8],
    needle: &[u8],
    case_sensitive: bool,
    unit: usize,
) -> Vec<usize> {
    literal_positions(haystack, needle, case_sensitive)
        .into_iter()
        .filter(|at| at.is_multiple_of(unit))
        .collect()
}

/// `read` が返す小さな窓だけを保持し、窓境界の最長パターン分だけ重ねる。
/// PieceTree のピース境界は行境界なので、検索語は境界を越えない（従来の
/// 行単位検索と同じ）。同じ行のバイトが複数ピースへ分割される構造は作られない。
#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn scan_encoded_range(
    len: usize,
    lines: usize,
    encoding: FileEncoding,
    delimiter: &[u8],
    query: &[u8],
    marker: &[u8],
    case_sensitive: bool,
    limit: usize,
    mut read: impl FnMut(usize, usize) -> Result<Vec<u8>, String>,
) -> Result<(Vec<RawScanHit>, usize), String> {
    let unit = encoding.unit_bytes();
    let cr = encoding.encode_str("\r");
    let query_has_delimiter = !aligned_positions(query, delimiter, true, unit).is_empty();
    let longest = query
        .len()
        .max(marker.len())
        .max(delimiter.len() + cr.len());
    let overlap = longest.saturating_sub(unit);
    let mut carry = Vec::new();
    let mut offset = 0;
    let mut processed = 0;
    let mut line = 0;
    let mut line_start = 0;
    let mut line_marker = false;
    let mut line_matches = Vec::new();
    let mut hits = Vec::new();
    let mut next_literal_end = 0;

    let finish_line = |content_end: usize,
                       line: usize,
                       line_start: usize,
                       line_marker: bool,
                       line_matches: &mut Vec<(usize, usize)>,
                       hits: &mut Vec<RawScanHit>| {
        if line_marker {
            hits.push(RawScanHit {
                line,
                notation: true,
                line_start,
                start: 0,
            });
        } else {
            for (start, end) in line_matches.drain(..) {
                if end <= content_end && hits.len() < limit {
                    hits.push(RawScanHit {
                        line,
                        notation: false,
                        line_start,
                        start,
                    });
                }
            }
        }
        line_matches.clear();
    };

    loop {
        let take = (len - offset).min(CHUNK);
        let part = read(offset, take)?;
        if part.len() != take {
            return Err("検索中に文書ストアの読み取り範囲が変わりました".to_string());
        }
        let data_start = offset.saturating_sub(carry.len());
        carry.extend_from_slice(&part);
        offset += take;
        let eof = offset == len;
        let safe_len = if eof {
            carry.len()
        } else {
            carry.len().saturating_sub(overlap) / unit * unit
        };
        let safe_end = data_start + safe_len;
        let query_positions = if !query_has_delimiter {
            aligned_positions(&carry, query, case_sensitive, unit)
        } else {
            Vec::new()
        };
        let marker_positions = aligned_positions(&carry, marker, true, unit);

        let has_query = query_positions.iter().any(|&at| {
            let absolute = data_start + at;
            absolute >= processed && absolute < safe_end && absolute >= next_literal_end
        });
        let has_marker = marker_positions.iter().any(|&at| {
            let absolute = data_start + at;
            absolute >= processed && absolute < safe_end
        });

        if !has_query && !has_marker {
            for at in aligned_positions(&carry, delimiter, true, unit) {
                let absolute = data_start + at;
                if absolute >= processed && absolute < safe_end {
                    line += 1;
                    line_start = absolute + delimiter.len();
                    if line >= lines {
                        return Ok((hits, line));
                    }
                }
            }
            processed = safe_end;
            if eof {
                break;
            }
            carry.drain(..safe_len);
            continue;
        }

        let mut events = Vec::new();
        for at in aligned_positions(&carry, delimiter, true, unit) {
            let absolute = data_start + at;
            if absolute >= processed && absolute < safe_end {
                events.push((absolute, 2_u8));
            }
        }
        for at in marker_positions {
            let absolute = data_start + at;
            if absolute >= processed && absolute < safe_end {
                events.push((absolute, 1_u8));
            }
        }
        for at in query_positions {
            let absolute = data_start + at;
            if absolute >= processed && absolute < safe_end && absolute >= next_literal_end {
                events.push((absolute, 0_u8));
            }
        }
        events.sort_unstable();
        for (at, kind) in events {
            match kind {
                0 => {
                    next_literal_end = at + query.len();
                    line_matches.push((at, next_literal_end));
                }
                1 => {
                    line_marker = true;
                    line_matches.clear();
                }
                _ => {
                    let content_end = if at - data_start >= cr.len()
                        && &carry[at - data_start - cr.len()..at - data_start] == cr.as_slice()
                    {
                        at - cr.len()
                    } else {
                        at
                    };
                    finish_line(
                        content_end,
                        line,
                        line_start,
                        line_marker,
                        &mut line_matches,
                        &mut hits,
                    );
                    line += 1;
                    line_start = at + delimiter.len();
                    line_marker = false;
                    if hits.len() >= limit || line >= lines {
                        return Ok((hits, line));
                    }
                }
            }
        }
        processed = safe_end;
        if eof {
            break;
        }
        carry.drain(..safe_len);
    }
    if line < lines {
        finish_line(
            len,
            line,
            line_start,
            line_marker,
            &mut line_matches,
            &mut hits,
        );
        line += 1;
    }
    Ok((hits, line.min(lines)))
}

/// 検索走査の 1 件。`notation` の行は一致ではなく「frontend が見るべき行」。
#[derive(serde::Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ScanHit {
    pub line: usize,
    pub notation: bool,
    /// 行内の一致の文字位置。`notation` の行では意味を持たない。
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct RawHit {
    pub(crate) hit: ScanHit,
    pub(crate) start_byte: usize,
    pub(crate) end_byte: usize,
}

use std::sync::Arc;

/// 検索パターンの完全抽象化 trait。
/// リテラル・正規表現・bi-gram 索引による絞り込み・推定件数取得を単一のインターフェースでカプセル化する。
pub(crate) trait DocumentMatcher: std::fmt::Debug + Send + Sync {
    /// 生バイト列 (mmap: &[u8]) に対する一致位置 (start_byte, end_byte) のイテレータ
    fn find_in_bytes<'a>(
        &'a self,
        haystack: &'a [u8],
        encoding: FileEncoding,
    ) -> Box<dyn Iterator<Item = (usize, usize)> + 'a>;

    /// 編集バッファ文字列 (&str) に対する一致位置 (start_char, end_char) のイテレータ
    fn find_in_str<'a>(&'a self, text: &'a str) -> Box<dyn Iterator<Item = (usize, usize)> + 'a>;

    /// bi-gram 索引により指定ブロックをスキップできるか判定
    fn may_contain_in_block(&self, index: &crate::search_index::SearchIndex, block: usize) -> bool {
        let _ = (index, block);
        true
    }

    /// bi-gram 索引から推定件数を取得できるか
    fn estimate_with_index(&self, index: &crate::search_index::SearchIndex) -> Option<usize> {
        let _ = index;
        None
    }

    /// クエリ文字数が固定（リテラルの長さ）であればその文字数を返す
    fn fixed_char_count(&self) -> Option<usize> {
        None
    }

    /// サンプリング等で使用する正規表現
    fn pattern(&self) -> &regex::Regex;

    /// 検索パターンの文字列表現（キャッシュキー用）
    fn pattern_str(&self) -> &str;

    /// リテラル文字列（リテラル検索の場合のみ Some）
    fn literal(&self) -> Option<&str> {
        None
    }

    /// 大小文字区別
    fn case_sensitive(&self) -> bool;

    /// 生バイト列内の一致件数を直接カウントする（サンプリングや高速推定用）
    fn count_in_bytes(&self, bytes: &[u8], encoding: FileEncoding) -> usize {
        self.find_in_bytes(bytes, encoding).count()
    }

    /// 文字列内の一致件数を直接カウントする
    fn count_in_str(&self, text: &str) -> usize {
        self.find_in_str(text).count()
    }

    /// mmap 生バイト列の全域（0〜EOF）から均等な窓（64窓）をサンプリングし、
    /// ファイル全体の一致件数を高精度かつ高速に推定する。
    fn estimate_from_bytes(&self, haystack: &[u8], encoding: FileEncoding) -> usize {
        const WINDOWS: usize = 64;
        const WINDOW_BYTES: usize = 32 * 1024; // 32KB
        let total_bytes = haystack.len();
        if total_bytes == 0 {
            return 0;
        }
        if total_bytes <= WINDOW_BYTES {
            return self.count_in_bytes(haystack, encoding);
        }
        let step = total_bytes.div_ceil(WINDOWS).max(WINDOW_BYTES);
        let take = WINDOW_BYTES;
        let mut sample_hits = 0;
        let mut sampled_bytes = 0;
        for from in (0..total_bytes).step_by(step) {
            let count = take.min(total_bytes - from);
            let window = &haystack[from..from + count];
            sample_hits += self.count_in_bytes(window, encoding);
            sampled_bytes += count;
        }
        if sampled_bytes == 0 {
            return 0;
        }
        ((sample_hits as u128 * total_bytes as u128 / sampled_bytes as u128) as usize)
            .min(total_bytes)
    }
}

#[derive(Debug)]
pub(crate) struct LiteralMatcher {
    query: String,
    case_sensitive: bool,
    pattern: regex::Regex,
    query_chars: usize,
}

impl LiteralMatcher {
    pub(crate) fn new(query: &str, case_sensitive: bool) -> Result<Self, String> {
        let pattern_str = regex::escape(query);
        let pattern = regex::RegexBuilder::new(&pattern_str)
            .case_insensitive(!case_sensitive)
            .build()
            .map_err(|e| format!("検索正規表現の生成に失敗しました: {e}"))?;
        let query_chars = query.chars().count();
        Ok(Self {
            query: query.to_string(),
            case_sensitive,
            pattern,
            query_chars,
        })
    }
}

impl DocumentMatcher for LiteralMatcher {
    fn find_in_bytes<'a>(
        &'a self,
        haystack: &'a [u8],
        encoding: FileEncoding,
    ) -> Box<dyn Iterator<Item = (usize, usize)> + 'a> {
        let encoded_query = encoding.encode_str(&self.query);
        let unit = encoding.unit_bytes();
        let query_len = encoded_query.len();
        if query_len == 0 {
            return Box::new(std::iter::empty());
        }

        let has_casing = self.query.chars().any(|c| {
            c.is_alphabetic()
                && (c.to_lowercase().collect::<String>() != c.to_uppercase().collect::<String>())
        });

        if self.case_sensitive || !has_casing {
            let matches: Vec<(usize, usize)> = memchr::memmem::find_iter(haystack, &encoded_query)
                .filter(move |&pos| unit == 1 || pos.is_multiple_of(unit))
                .map(move |pos| (pos, pos + query_len))
                .collect();
            Box::new(matches.into_iter())
        } else if unit == 1 {
            let positions = literal_positions(haystack, &encoded_query, false);
            Box::new(positions.into_iter().map(move |pos| (pos, pos + query_len)))
        } else if let Ok(re) = regex::bytes::RegexBuilder::new(&regex::escape(&self.query))
            .case_insensitive(true)
            .build()
        {
            let matches: Vec<(usize, usize)> = re
                .find_iter(haystack)
                .filter(|m| unit == 1 || m.start().is_multiple_of(unit))
                .map(|m| (m.start(), m.end()))
                .collect();
            Box::new(matches.into_iter())
        } else {
            Box::new(std::iter::empty())
        }
    }

    fn find_in_str<'a>(&'a self, text: &'a str) -> Box<dyn Iterator<Item = (usize, usize)> + 'a> {
        let query_len = self.query_chars;
        if self.case_sensitive {
            let query = self.query.clone();
            let matches: Vec<(usize, usize)> = text
                .match_indices(&query)
                .map(|(byte_offset, _)| {
                    let start = text[..byte_offset].chars().count();
                    (start, start + query_len)
                })
                .collect();
            Box::new(matches.into_iter())
        } else {
            let matches: Vec<(usize, usize)> = self
                .pattern
                .find_iter(text)
                .map(|m| {
                    let start = text[..m.start()].chars().count();
                    let end = start + text[m.start()..m.end()].chars().count();
                    (start, end)
                })
                .collect();
            Box::new(matches.into_iter())
        }
    }

    fn may_contain_in_block(&self, index: &crate::search_index::SearchIndex, block: usize) -> bool {
        index.may_contain_query(block, &self.query)
    }

    fn estimate_with_index(&self, index: &crate::search_index::SearchIndex) -> Option<usize> {
        index.estimate_matches(&self.query)
    }

    fn fixed_char_count(&self) -> Option<usize> {
        Some(self.query_chars)
    }

    fn pattern(&self) -> &regex::Regex {
        &self.pattern
    }

    fn pattern_str(&self) -> &str {
        &self.query
    }

    fn literal(&self) -> Option<&str> {
        Some(&self.query)
    }

    fn case_sensitive(&self) -> bool {
        self.case_sensitive
    }

    fn count_in_bytes(&self, bytes: &[u8], encoding: FileEncoding) -> usize {
        let encoded_query = encoding.encode_str(&self.query);
        let unit = encoding.unit_bytes();
        let query_len = encoded_query.len();
        if query_len == 0 {
            return 0;
        }

        let has_casing = self.query.chars().any(|c| {
            c.is_alphabetic()
                && (c.to_lowercase().collect::<String>() != c.to_uppercase().collect::<String>())
        });

        if self.case_sensitive || !has_casing {
            memchr::memmem::find_iter(bytes, &encoded_query)
                .filter(|&pos| unit == 1 || pos.is_multiple_of(unit))
                .count()
        } else if unit == 1 {
            literal_positions(bytes, &encoded_query, false).len()
        } else if let Ok(re) = regex::bytes::RegexBuilder::new(&regex::escape(&self.query))
            .case_insensitive(true)
            .build()
        {
            re.find_iter(bytes)
                .filter(|m| unit == 1 || m.start().is_multiple_of(unit))
                .count()
        } else {
            0
        }
    }

    fn count_in_str(&self, text: &str) -> usize {
        if self.case_sensitive {
            text.match_indices(&self.query).count()
        } else {
            self.pattern.find_iter(text).count()
        }
    }
}

#[derive(Debug)]
pub(crate) struct RegexMatcher {
    query: String,
    pattern: regex::Regex,
    bytes_regex: Option<regex::bytes::Regex>,
    case_sensitive: bool,
}

impl RegexMatcher {
    pub(crate) fn new(query: &str, case_sensitive: bool) -> Result<Self, String> {
        let pattern = regex::RegexBuilder::new(query)
            .case_insensitive(!case_sensitive)
            .build()
            .map_err(|e| format!("正規表現を読めませんでした: {e}"))?;
        let bytes_regex = regex::bytes::RegexBuilder::new(query)
            .case_insensitive(!case_sensitive)
            .build()
            .ok();
        Ok(Self {
            query: query.to_string(),
            pattern,
            bytes_regex,
            case_sensitive,
        })
    }
}

impl DocumentMatcher for RegexMatcher {
    fn find_in_bytes<'a>(
        &'a self,
        haystack: &'a [u8],
        encoding: FileEncoding,
    ) -> Box<dyn Iterator<Item = (usize, usize)> + 'a> {
        if matches!(encoding, FileEncoding::Utf8 | FileEncoding::Utf8Bom) {
            if let Some(ref re) = self.bytes_regex {
                let matches: Vec<(usize, usize)> =
                    re.find_iter(haystack).map(|m| (m.start(), m.end())).collect();
                return Box::new(matches.into_iter());
            }
        }
        Box::new(std::iter::empty())
    }

    fn find_in_str<'a>(&'a self, text: &'a str) -> Box<dyn Iterator<Item = (usize, usize)> + 'a> {
        let matches: Vec<(usize, usize)> = self
            .pattern
            .find_iter(text)
            .map(|m| {
                let start = text[..m.start()].chars().count();
                let end = start + text[m.start()..m.end()].chars().count();
                (start, end)
            })
            .collect();
        Box::new(matches.into_iter())
    }

    fn pattern(&self) -> &regex::Regex {
        &self.pattern
    }

    fn pattern_str(&self) -> &str {
        &self.query
    }

    fn literal(&self) -> Option<&str> {
        None
    }

    fn case_sensitive(&self) -> bool {
        self.case_sensitive
    }

    fn count_in_bytes(&self, bytes: &[u8], encoding: FileEncoding) -> usize {
        if matches!(encoding, FileEncoding::Utf8 | FileEncoding::Utf8Bom) {
            if let Some(ref re) = self.bytes_regex {
                return re.find_iter(bytes).count();
            }
        }
        0
    }

    fn count_in_str(&self, text: &str) -> usize {
        self.pattern.find_iter(text).count()
    }

    fn estimate_with_index(&self, index: &crate::search_index::SearchIndex) -> Option<usize> {
        index.estimate_matches(&self.query)
    }
}

impl DocumentMatcher for regex::Regex {
    fn find_in_bytes<'a>(
        &'a self,
        haystack: &'a [u8],
        encoding: FileEncoding,
    ) -> Box<dyn Iterator<Item = (usize, usize)> + 'a> {
        if matches!(encoding, FileEncoding::Utf8 | FileEncoding::Utf8Bom) {
            if let Ok(re) = regex::bytes::RegexBuilder::new(self.as_str()).build() {
                let matches: Vec<(usize, usize)> =
                    re.find_iter(haystack).map(|m| (m.start(), m.end())).collect();
                return Box::new(matches.into_iter());
            }
        }
        Box::new(std::iter::empty())
    }

    fn find_in_str<'a>(&'a self, text: &'a str) -> Box<dyn Iterator<Item = (usize, usize)> + 'a> {
        let matches: Vec<(usize, usize)> = self
            .find_iter(text)
            .map(|m| {
                let start = text[..m.start()].chars().count();
                let end = start + text[m.start()..m.end()].chars().count();
                (start, end)
            })
            .collect();
        Box::new(matches.into_iter())
    }

    fn pattern(&self) -> &regex::Regex {
        self
    }

    fn pattern_str(&self) -> &str {
        self.as_str()
    }

    fn case_sensitive(&self) -> bool {
        true
    }

    fn count_in_bytes(&self, bytes: &[u8], encoding: FileEncoding) -> usize {
        if matches!(encoding, FileEncoding::Utf8 | FileEncoding::Utf8Bom) {
            if let Ok(re) = regex::bytes::RegexBuilder::new(self.as_str()).build() {
                return re.find_iter(bytes).count();
            }
        }
        0
    }

    fn count_in_str(&self, text: &str) -> usize {
        self.find_iter(text).count()
    }

    fn estimate_with_index(&self, index: &crate::search_index::SearchIndex) -> Option<usize> {
        index.estimate_matches(self.as_str())
    }
}

#[derive(Clone, Debug)]
pub struct CompiledQuery {
    pub(crate) matcher: Arc<dyn DocumentMatcher>,
    pub marker: char,
}

impl CompiledQuery {
    pub fn compile(
        query: &str,
        regex: bool,
        case_sensitive: bool,
        marker: char,
    ) -> Result<Self, String> {
        let matcher: Arc<dyn DocumentMatcher> = if regex {
            Arc::new(RegexMatcher::new(query, case_sensitive)?)
        } else {
            Arc::new(LiteralMatcher::new(query, case_sensitive)?)
        };
        Ok(Self { matcher, marker })
    }

    pub fn pattern(&self) -> &regex::Regex {
        self.matcher.pattern()
    }

    pub fn literal(&self) -> Option<&str> {
        self.matcher.literal()
    }

    pub fn case_sensitive(&self) -> bool {
        self.matcher.case_sensitive()
    }
}

pub(crate) struct SearchCandidates {
    pub(crate) hits: Vec<ScanHit>,
    pub(crate) scanned_to: usize,
    pub(crate) cancelled: bool,
    pub(crate) total_matches: Option<usize>,
    pub(crate) current_index: Option<usize>,
}

pub(crate) struct SearchSpec<'a> {
    pub(crate) query: &'a CompiledQuery,
    pub(crate) from: usize,
    pub(crate) end: usize,
    pub(crate) after_col: Option<usize>,
    pub(crate) forward: bool,
}

impl Document {
    /// クリーン／編集済み（dirty）を問わず、元ファイル部分を mmap ゼロコピー走査し、
    /// 操作ログ (OperationLog::map_range) による現在座標写像と編集バッファ差分走査を
    /// 完全に単一のパイプラインとして解決する。
    pub(crate) fn resolve_hits(
        &mut self,
        matcher: &dyn DocumentMatcher,
        marker: char,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<Vec<ScanHit>, String> {
        let is_clean = self.is_clean();
        let mut all_hits = Vec::new();

        if let Some(ref source) = self.source {
            let raw_hits = source.scan_matches(
                matcher,
                self.search_index(),
                marker,
                usize::MAX,
                cancelled,
            )?;
            if is_clean {
                all_hits = raw_hits.into_iter().map(|r| r.hit).collect();
            } else {
                for raw in raw_hits {
                    if cancelled() {
                        return Ok(Vec::new());
                    }
                    if let Ok((new_start, new_end)) =
                        self.map_range_from_base(raw.start_byte, raw.end_byte)
                    {
                        if let (Some((line1, col1)), Some((line2, col2))) = (
                            self.byte_offset_to_line_column(new_start),
                            self.byte_offset_to_line_column(new_end),
                        ) {
                            if line1 == line2 {
                                all_hits.push(ScanHit {
                                    line: line1,
                                    notation: raw.hit.notation,
                                    start: col1,
                                    end: col2,
                                });
                            }
                        }
                    }
                }

                // 編集バッファ（変更行）の差分走査
                let mod_lines = self.modified_lines();
                for line_idx in mod_lines {
                    if cancelled() {
                        return Ok(Vec::new());
                    }
                    self.each_line(line_idx, 1, &mut |_, line| {
                        let has_marker = marker != '\0' && line.contains(marker);
                        if has_marker {
                            all_hits.push(ScanHit {
                                line: line_idx,
                                notation: true,
                                start: 0,
                                end: 0,
                            });
                        } else {
                            for (start, end) in matcher.find_in_str(line) {
                                all_hits.push(ScanHit {
                                    line: line_idx,
                                    notation: false,
                                    start,
                                    end,
                                });
                            }
                        }
                        true
                    })?;
                }

                all_hits.sort_by_key(|h| (h.line, h.start));
                all_hits.dedup();
            }
        } else {
            // source なしのメモリ内文書（新規下書きなど）
            self.each_line(0, self.line_count(), &mut |line_idx, line| {
                if cancelled() {
                    return false;
                }
                let has_marker = marker != '\0' && line.contains(marker);
                if has_marker {
                    all_hits.push(ScanHit {
                        line: line_idx,
                        notation: true,
                        start: 0,
                        end: 0,
                    });
                } else {
                    for (start, end) in matcher.find_in_str(line) {
                        all_hits.push(ScanHit {
                            line: line_idx,
                            notation: false,
                            start,
                            end,
                        });
                    }
                }
                true
            })?;
        }

        Ok(all_hits)
    }

    /// `from..end` の範囲を指定された方向（forward）に走査し、候補群を返す。
    /// 一致が見つかり次第、即座に返却する。
    pub(crate) fn search_candidates(
        &mut self,
        spec: SearchSpec<'_>,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<SearchCandidates, String> {
        let from = spec.from;
        if cancelled() {
            return Ok(SearchCandidates {
                hits: Vec::new(),
                scanned_to: from,
                cancelled: true,
                total_matches: None,
                current_index: None,
            });
        }

        let cur_rev = self.revision();
        let pattern_str = spec.query.matcher.pattern_str();
        let literal = spec.query.matcher.literal();
        let case_sensitive = spec.query.matcher.case_sensitive();
        let marker = spec.query.marker;
        let end = spec.end;
        let after_col = spec.after_col;
        let forward = spec.forward;

        // 1. キャッシュの初期化または再利用
        let is_cached = self.search_cache.as_ref().map_or(false, |cache| {
            cache.revision == cur_rev
                && cache.case_sensitive == case_sensitive
                && cache.pattern == pattern_str
                && cache.literal.as_deref() == literal
                && cache.fully_scanned
        });

        if !is_cached {
            if cancelled() {
                return Ok(SearchCandidates {
                    hits: Vec::new(),
                    scanned_to: from,
                    cancelled: true,
                    total_matches: None,
                    current_index: None,
                });
            }

            let all_hits = self.resolve_hits(spec.query.matcher.as_ref(), marker, cancelled)?;

            if cancelled() {
                return Ok(SearchCandidates {
                    hits: Vec::new(),
                    scanned_to: from,
                    cancelled: true,
                    total_matches: None,
                    current_index: None,
                });
            }

            self.search_cache = Some(SearchHitCache {
                pattern: pattern_str.to_string(),
                literal: literal.map(|s| s.to_string()),
                case_sensitive,
                revision: cur_rev,
                hits: all_hits,
                fully_scanned: true,
            });
        }

        let cache = self.search_cache.as_ref().unwrap();
        let total = cache.hits.len();
        if total == 0 {
            return Ok(SearchCandidates {
                hits: Vec::new(),
                scanned_to: end,
                cancelled: false,
                total_matches: Some(0),
                current_index: None,
            });
        }

        let target = (from, after_col.unwrap_or(0));

        let (cur_idx, hits) = if forward {
            let i = cache.hits.partition_point(|h| (h.line, h.start) < target);
            if i < total {
                let take = (total - i).min(64);
                (i + 1, cache.hits[i..i + take].to_vec())
            } else {
                let take = total.min(64);
                (1, cache.hits[..take].to_vec())
            }
        } else {
            let i = cache.hits.partition_point(|h| (h.line, h.start) < target);
            if i > 0 {
                (i, vec![cache.hits[i - 1].clone()])
            } else {
                (total, vec![cache.hits[total - 1].clone()])
            }
        };

        Ok(SearchCandidates {
            hits,
            scanned_to: end,
            cancelled: false,
            total_matches: Some(total),
            current_index: Some(cur_idx),
        })
    }

    /// 検索の走査で見つかったもの: 素の行の一致か、読み替え（記法の解釈）を
    /// 要する行。行の順に並ぶ。
    #[allow(dead_code)]
    pub(crate) fn scan(
        &mut self,
        pattern: &regex::Regex,
        needle: char,
        from: usize,
        count: usize,
        limit: usize,
    ) -> Result<(Vec<ScanHit>, usize), String> {
        let mut hits = Vec::new();
        let mut scanned_to = from.saturating_add(count).min(self.line_count());
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
                false
            } else {
                true
            }
        })?;
        Ok((hits, scanned_to))
    }

    pub(crate) fn estimate_matches(
        &mut self,
        matcher: &dyn DocumentMatcher,
    ) -> Result<usize, String> {
        self.confirm_scan_if_done();
        let cur_rev = self.revision();
        let pattern_str = matcher.pattern_str();

        if let Some(cache) = &self.search_cache {
            if cache.fully_scanned && cache.revision == cur_rev && cache.pattern == pattern_str {
                return Ok(cache.hits.len());
            }
        }

        if let Some(index) = self.search_index() {
            if let Some(estimated) = matcher.estimate_with_index(index) {
                return Ok(estimated);
            }
        }

        if let Some(ref source) = self.source {
            if self.is_clean() {
                if let Ok(mmap) = source.mmap() {
                    let offset = source.content_offset as usize;
                    if offset < mmap.len() {
                        return Ok(matcher.estimate_from_bytes(&mmap[offset..], source.encoding));
                    }
                }
            }
        }

        let effective_count = self.estimated_line_count();

        const WINDOWS: usize = 64;
        const LINES_PER_WINDOW: usize = 2_000;
        let step = self.line_count().div_ceil(WINDOWS).max(1);
        let take = LINES_PER_WINDOW.min(step);
        let mut hits = 0;
        let mut sampled = 0;
        for from in (0..self.line_count()).step_by(step) {
            let count = take.min(self.line_count() - from);
            self.each_line(from, count, &mut |_, line| {
                hits += matcher.count_in_str(line);
                sampled += 1;
                true
            })?;
        }
        if sampled == 0 {
            return Ok(0);
        }
        // 切り上げ丸めは全行ヒット時に count+1 を返してしまうので、切り捨てで推定する。
        Ok(
            ((hits as u128 * effective_count as u128 / sampled as u128) as usize)
                .min(effective_count),
        )
    }

    /// `from..=to` の行のうち `needle` を含むもの。
    pub(crate) fn lines_containing(
        &mut self,
        from: usize,
        to: usize,
        needle: char,
    ) -> Result<Vec<usize>, String> {
        let to = to.min(self.line_count().saturating_sub(1));
        let mut found = Vec::new();
        self.each_line(from, to.saturating_sub(from) + 1, &mut |i, line| {
            if line.contains(needle) {
                found.push(i);
            }
            true
        })?;
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::character_columns;
    use crate::source::FileEncoding;

    #[test]
    fn character_columns_process_each_prefix_byte_once() {
        let repeated = format!("{}needle", "x".repeat(26)).repeat(1_000);
        let text = format!("前{repeated}");
        let bytes = text.as_bytes();
        let positions = super::literal_positions(bytes, b"needle", true);
        let mut bytes_read = 0;
        let columns = character_columns(FileEncoding::Utf8, &positions, |from, len| {
            bytes_read += len;
            Ok(bytes[from..from + len].to_vec())
        })
        .unwrap();

        assert_eq!(columns[0], 27);
        assert_eq!(columns[999], 31_995);
        assert_eq!(bytes_read, *positions.last().unwrap());
    }

    #[test]
    fn progressive_columns_keep_supported_encoding_boundaries() {
        for encoding in [
            FileEncoding::Utf8,
            FileEncoding::ShiftJis,
            FileEncoding::EucJp,
            FileEncoding::Iso2022Jp,
            FileEncoding::Utf16Le,
            FileEncoding::Utf16Be,
        ] {
            let bytes = encoding.encode_str("前needle後needle");
            let query = encoding.encode_str("needle");
            let positions = super::aligned_positions(&bytes, &query, true, encoding.unit_bytes());
            let columns = character_columns(encoding, &positions, |from, len| {
                Ok(bytes[from..from + len].to_vec())
            })
            .unwrap();
            assert_eq!(columns, vec![1, 8], "{encoding:?}");
        }
    }

    use crate::search::{literal_positions, SearchSpec};
    use crate::source::{LineEnding, CHUNK, MAX_LINE_BYTES};
    use crate::test_utils::*;

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


    /// 【回帰防止テスト】
    /// バックグラウンド走査中の巨大ファイル（pending_source が残る状態）であっても、
    /// estimate_matches は最初のチャンク行数で頭打ちにならず、ファイル全体のサイズに
    /// 正しく外挿された推定件数を返すことを保証する。
    /// また、走査が完了した後は自動的に confirm_scan_if_done が走り、確定件数が返ることを保証する。
    #[test]
    fn estimate_matches_extrapolates_pending_source_and_confirms_when_done() {
        let lines: Vec<String> = (0..100_000)
            .map(|i| format!("line {i} with keyword target"))
            .collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let (mut doc, path) = disk_doc("estimate-pending-extrapolate", &refs);
        let pattern = regex::Regex::new("target").unwrap();

        let initial_lines = 1_000;
        let initial_bytes = doc.pieces_byte_offset_for_test(initial_lines);
        let total_bytes = doc.pieces_byte_offset_for_test(100_000);

        doc.simulate_pending_source_for_test(initial_bytes, initial_lines, total_bytes);

        // 未確定状態（count = 1,000）でも、ファイル全体規模（約100,000件）に外挿されること
        let estimated = doc.estimate_matches(&pattern).unwrap();
        assert!(
            (90_000..=110_000).contains(&estimated),
            "未確定走査中でもファイル全体規模に外挿されること: expected ~100000, got {estimated}"
        );

        // 走査完了をシミュレート
        doc.simulate_scan_done_for_test(100_000);

        // 走査完了後は confirm_scan_if_done が自動で走り、count が 100,000 に確定すること
        let final_estimated = doc.estimate_matches(&pattern).unwrap();
        assert!(
            (final_estimated as isize - 100_000).abs() <= 10,
            "走査完了後の推定件数は約100,000件であること: got {final_estimated}"
        );
        assert_eq!(doc.line_count(), 100_000);

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
        let query = crate::search::CompiledQuery::compile("first", false, true, '$').unwrap();
        let found = snapshot
            .search_candidates(
                SearchSpec {
                    query: &query,
                    from: 0,
                    end: 2,
                    after_col: None,
                    forward: true,
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
        let query = crate::search::CompiledQuery::compile("missing", false, true, '$').unwrap();
        let found = snapshot
            .search_candidates(
                SearchSpec {
                    query: &query,
                    from: 0,
                    end: 2,
                    after_col: None,
                    forward: true,
                },
                &|| true,
            )
            .unwrap();
        assert!(found.cancelled);
        assert!(found.hits.is_empty());
        std::fs::remove_file(path).ok();
    }


    #[test]
    fn literal_search_candidates_resume_after_a_character_column() {
        let (doc, path) = disk_doc("literal-search-resume", &["前needle後needle", "needle"]);
        let mut snapshot = doc.search_snapshot().unwrap();
        let query = crate::search::CompiledQuery::compile("needle", false, true, '$').unwrap();
        let found = snapshot
            .search_candidates(
                SearchSpec {
                    query: &query,
                    from: 0,
                    end: 2,
                    after_col: Some(8),
                    forward: true,
                },
                &|| false,
            )
            .unwrap();

        assert!(!found.cancelled);
        assert_eq!(found.scanned_to, 2);
        assert_eq!(
            found
                .hits
                .iter()
                .map(|hit| (hit.line, hit.start, hit.end))
                .collect::<Vec<_>>(),
            vec![(0, 8, 14), (1, 0, 6)]
        );
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
    fn literal_scan_handles_a_source_line_over_the_display_limit() {
        let prefix = "x".repeat(MAX_LINE_BYTES + 1);
        let line = format!("{prefix}needle");
        let (mut doc, path) = disk_doc("literal-over-display-limit", &[&line]);

        let (hits, scanned_to) = doc.scan_literal("needle", true, '$', 0, 1, 64).unwrap();

        assert_eq!(scanned_to, 1);
        assert_eq!(
            hits.iter()
                .map(|hit| (hit.line, hit.notation, hit.start, hit.end))
                .collect::<Vec<_>>(),
            vec![(0, false, MAX_LINE_BYTES + 1, MAX_LINE_BYTES + 7)]
        );
        std::fs::remove_file(path).ok();
    }


    #[test]
    fn literal_scan_finds_a_match_across_its_source_chunk_boundary() {
        let line = format!("{}needle", "x".repeat(CHUNK - 3));
        let (mut doc, path) = disk_doc("literal-chunk-boundary", &[&line]);

        let (hits, _) = doc.scan_literal("needle", true, '$', 0, 1, 64).unwrap();

        assert_eq!(hits.len(), 1);
        assert_eq!(
            (hits[0].line, hits[0].start, hits[0].end),
            (0, CHUNK - 3, CHUNK + 3)
        );
        std::fs::remove_file(path).ok();
    }


    #[test]
    fn literal_scan_keeps_utf16_alignment_and_character_columns() {
        for (encoding, alignment_line, alignment_query) in [
            (FileEncoding::Utf16Le, "\0\u{1}Ā", "Ā"),
            (FileEncoding::Utf16Be, "ĀĀ\u{1}", "\u{1}"),
        ] {
            let (mut doc, path) = encoded_disk_doc(
                &format!("literal-{encoding:?}"),
                &["前😀needle後", "needle", alignment_line],
                encoding,
                LineEnding::Lf,
                false,
            );

            let (hits, _) = doc.scan_literal("needle", true, '$', 0, 3, 64).unwrap();
            assert_eq!(
                hits.iter()
                    .map(|hit| (hit.line, hit.start, hit.end))
                    .collect::<Vec<_>>(),
                vec![(0, 2, 8), (1, 0, 6)]
            );
            let (aligned, _) = doc
                .scan_literal(alignment_query, true, '$', 2, 1, 64)
                .unwrap();
            assert_eq!(
                aligned
                    .iter()
                    .map(|hit| (hit.line, hit.start, hit.end))
                    .collect::<Vec<_>>(),
                vec![(2, 2, 3)]
            );
            std::fs::remove_file(path).ok();
        }
    }


    #[test]
    fn literal_scan_searches_edited_pieces_and_keeps_global_lines() {
        let (mut doc, path) = disk_doc("literal-edited-piece", &["zero", "one", "two", "three"]);
        doc.replace(
            1,
            3,
            vec![
                "edited needle".into(),
                "middle".into(),
                "needle tail".into(),
            ],
            1,
            "",
            "",
        )
        .unwrap();

        let (hits, _) = doc
            .scan_literal("needle", true, '$', 0, doc.line_count(), 64)
            .unwrap();

        assert_eq!(
            hits.iter()
                .map(|hit| (hit.line, hit.start, hit.end))
                .collect::<Vec<_>>(),
            vec![(1, 7, 13), (3, 0, 6)]
        );
        std::fs::remove_file(path).ok();
    }


    #[test]
    fn notation_marker_suppresses_all_literal_hits_on_its_line() {
        let (mut doc, path) = disk_doc("literal-notation", &["needle $ needle", "needle"]);

        let (hits, _) = doc.scan_literal("needle", true, '$', 0, 2, 64).unwrap();

        assert_eq!(
            hits.iter()
                .map(|hit| (hit.line, hit.notation, hit.start, hit.end))
                .collect::<Vec<_>>(),
            vec![(0, true, 0, 0), (1, false, 0, 6)]
        );
        std::fs::remove_file(path).ok();
    }


    #[test]
    fn literal_scan_small_limit_counts_a_marker_line_once() {
        let (mut doc, path) = disk_doc(
            "literal-small-limit-marker",
            &["needle $ needle", "前needle後needle", "needle"],
        );

        let (hits, scanned_to) = doc.scan_literal("needle", true, '$', 0, 3, 2).unwrap();

        assert_eq!(scanned_to, 2);
        assert_eq!(
            hits.iter()
                .map(|hit| (hit.line, hit.notation, hit.start, hit.end))
                .collect::<Vec<_>>(),
            vec![(0, true, 0, 0), (1, false, 1, 7)]
        );
        std::fs::remove_file(path).ok();
    }


    #[test]
    fn literal_scan_line_numbers_are_correct_across_source_and_edit_pieces() {
        let (mut doc, path) = disk_doc(
            "literal-multiple-pieces",
            &["needle 0", "one", "two", "three", "needle 4"],
        );
        doc.replace(2, 3, vec!["needle 2".into()], 1, "", "")
            .unwrap();

        let (hits, scanned_to) = doc.scan_literal("needle", true, '$', 0, 5, 64).unwrap();

        assert_eq!(scanned_to, 5);
        assert_eq!(
            hits.iter().map(|hit| hit.line).collect::<Vec<_>>(),
            vec![0, 2, 4]
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


    #[test]
    fn search_snapshot_maps_hits_across_intervening_edits() {
        let (mut doc, path) = disk_doc("search-map", &["apple", "banana", "cherry"]);
        let snap = doc.search_snapshot().unwrap();
        // 検索スナップショット取得後に先頭に 1 行挿入
        doc.replace(0, 0, vec!["prefix".to_string()], 1, "", "")
            .unwrap();
        assert_eq!(doc.line_count(), 4);

        // スナップショット時点では "cherry" は 2 行目
        let hits = vec![crate::search::ScanHit {
            line: 2,
            notation: false,
            start: 0,
            end: 6,
        }];
        let mapped = doc.map_search_hits(&snap, hits);
        assert_eq!(mapped.len(), 1);
        // 現在の文書では 3 行目へ写像されていること
        assert_eq!(mapped[0].line, 3);
        assert_eq!(mapped[0].start, 0);
        assert_eq!(mapped[0].end, 6);
        std::fs::remove_file(path).ok();
    }


    #[test]
    fn search_snapshot_invalidates_hits_overlapping_edits() {
        let (mut doc, path) = disk_doc("search-invalid", &["apple", "banana", "cherry"]);
        let snap = doc.search_snapshot().unwrap();
        // 検索スナップショット取得後に "banana"（1行目）を削除
        doc.replace(1, 2, Vec::new(), 1, "", "").unwrap();
        assert_eq!(doc.line_count(), 2);

        // スナップショット時点での "banana" ヒット
        let hits = vec![crate::search::ScanHit {
            line: 1,
            notation: false,
            start: 0,
            end: 6,
        }];
        let mapped = doc.map_search_hits(&snap, hits);
        // 削除範囲と重なっているため無効化されること
        assert!(mapped.is_empty());
        std::fs::remove_file(path).ok();
    }


    #[test]
    fn search_index_estimates_matches_and_updates_on_edit() {
        let (mut doc, path) = disk_doc(
            "index-test",
            &["apple banana", "orange apple", "grape apple"],
        );
        doc.enable_search_index();

        // 初期状態で "apple" が 3 件推定されること
        let re = regex::Regex::new("apple").unwrap();
        assert_eq!(doc.estimate_matches(&re).unwrap(), 3);

        // 1行目を置換して "apple" を減らす
        doc.replace(
            0,
            1,
            vec!["kiwi banana".to_string()],
            1,
            "apple banana",
            "kiwi banana",
        )
        .unwrap();

        // 差分キャッシュにより推定件数が 2 件になること
        assert_eq!(doc.estimate_matches(&re).unwrap(), 2);

        std::fs::remove_file(path).ok();
    }


    #[test]
    fn search_index_treats_structure_format_as_raw_text() {
        // 構造フォーマット・数式記法を含むテキストもフィルタリングせずそのまま bi-gram インデックス化されること
        let (mut doc, path) = disk_doc(
            "index-notation-test",
            &["formula: $(E = mc^2)$", "text line", "another $(x + y)$"],
        );
        doc.enable_search_index();

        let re = regex::Regex::new(r"\$\(").unwrap();
        // 記法プレフィックス "$(" を含む箇所が 2 件推定されること
        assert_eq!(doc.estimate_matches(&re).unwrap(), 2);

        std::fs::remove_file(path).ok();
    }


    #[test]
    fn search_index_undo_redo_updates_deltas() {
        let (mut doc, path) = disk_doc(
            "index-undo-test",
            &["apple banana", "orange apple", "grape apple"],
        );
        doc.enable_search_index();

        let re = regex::Regex::new("apple").unwrap();
        assert_eq!(doc.estimate_matches(&re).unwrap(), 3);

        // 1行目を置換
        doc.replace(
            0,
            1,
            vec!["kiwi banana".to_string()],
            1,
            "apple banana",
            "kiwi banana",
        )
        .unwrap();
        assert_eq!(doc.estimate_matches(&re).unwrap(), 2);

        // undo で差分キャッシュが元に戻り、3 件になること
        doc.undo().unwrap();
        assert_eq!(doc.estimate_matches(&re).unwrap(), 3);

        // redo で再度 2 件になること
        doc.redo().unwrap();
        assert_eq!(doc.estimate_matches(&re).unwrap(), 2);

        std::fs::remove_file(path).ok();
    }


    #[test]
    fn search_candidates_finds_distant_matches_efficiently() {
        use crate::search::{CompiledQuery, SearchSpec};

        // 512KB を超える複数ブロックのファイルを作成し、遠いブロックのみにターゲットを配置
        let mut lines = Vec::new();
        // 1行約100バイト * 15,000行 = 約1.5MB（約3ブロック分）
        let padding = "x".repeat(95);
        for _ in 0..15_000 {
            lines.push(padding.clone());
        }
        lines.push("hello target_needle world".to_string());
        lines.push(padding);

        let lines_ref: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        let (mut doc, path) = disk_doc("distant-block-test", &lines_ref);
        doc.enable_search_index();

        let compiled = CompiledQuery::compile("target_needle", false, true, '\0').unwrap();
        let spec = SearchSpec {
            query: &compiled,
            from: 0,
            end: doc.line_count(),
            after_col: None,
            forward: true,
        };
        let candidates = doc.search_candidates(spec, &|| false).unwrap();
        assert!(!candidates.cancelled);
        assert_eq!(candidates.hits.len(), 1);
        assert_eq!(candidates.hits[0].line, 15_000);
        assert_eq!(candidates.hits[0].start, 6);

        std::fs::remove_file(path).ok();
    }


    /// 回帰: 1 行に 64 件を超える一致がある場合でも、after_col の後ろの
    /// 一致が切り捨てられずに返ることを検証する。
    #[test]
    fn search_returns_matches_beyond_64_within_a_line() {
        // 1 行に 70 個の "m" を含む行を作る
        let many = "m ".repeat(70);
        let (mut doc, path) = disk_doc("many-matches", &[&many, "tail"]);
        let query = crate::search::CompiledQuery::compile("m", false, true, '$').unwrap();

        // after_col を 65 件目のあたりへ置いて検索し、残りの一致が返ること
        let found = doc
            .search_candidates(
                SearchSpec {
                    query: &query,
                    from: 0,
                    end: 2,
                    after_col: Some(130),
                    forward: true,
                },
                &|| false,
            )
            .unwrap();
        assert!(
            !found.hits.is_empty(),
            "after_col 以降の一致が返ること（64件上限で潰れない）"
        );
        // 返った一致はすべて after_col 以降
        for hit in &found.hits {
            assert!(hit.notation || hit.line > 0 || hit.start >= 130);
        }
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn test_dirty_document_search_cache_and_wraparound() {
        let (mut doc, path) = disk_doc("dirty-cache-test", &["alpha", "beta", "gamma", "delta"]);
        // 編集を加えて dirty (!is_clean) にする
        doc.replace(1, 2, vec!["edited_target".to_string()], 1, "", "").unwrap();
        assert!(!doc.is_clean());

        let query = crate::search::CompiledQuery::compile("target", false, true, '$').unwrap();

        // 1回目: 走査して見つかる -> キャッシュに入る
        let found1 = doc.search_candidates(
            SearchSpec {
                query: &query,
                from: 0,
                end: doc.line_count(),
                after_col: None,
                forward: true,
            },
            &|| false,
        ).unwrap();
        assert_eq!(found1.hits.len(), 1);
        assert_eq!(found1.hits[0].line, 1);
        assert_eq!(found1.hits[0].start, 7);

        // キャッシュに保存されていることを確認
        assert!(doc.search_cache.is_some());
        let cache = doc.search_cache.as_ref().unwrap();
        assert_eq!(cache.hits.len(), 1);

        // 2回目（forward、同一位置以降）: キャッシュから返る
        let found2 = doc.search_candidates(
            SearchSpec {
                query: &query,
                from: 1,
                end: doc.line_count(),
                after_col: Some(7),
                forward: true,
            },
            &|| false,
        ).unwrap();
        assert_eq!(found2.hits.len(), 1);
        assert_eq!(found2.hits[0].line, 1);

        // backward（末尾から手前へ）: キャッシュから返る
        let found_back = doc.search_candidates(
            SearchSpec {
                query: &query,
                from: 3,
                end: doc.line_count(),
                after_col: None,
                forward: false,
            },
            &|| false,
        ).unwrap();
        assert_eq!(found_back.hits.len(), 1);
        assert_eq!(found_back.hits[0].line, 1);

        // さらに末尾からの forward（ラップアラウンド）
        let found_wrap = doc.search_candidates(
            SearchSpec {
                query: &query,
                from: 3,
                end: doc.line_count(),
                after_col: None,
                forward: true,
            },
            &|| false,
        ).unwrap();
        // 現在位置 3 以降には無いが、先頭から検索し直して line 1 のヒットが返る
        assert_eq!(found_wrap.hits.len(), 1);
        assert_eq!(found_wrap.hits[0].line, 1);

        std::fs::remove_file(path).ok();
    }

    #[test]
    fn test_previous_from_last_hit_repro() {
        let mut lines = Vec::new();
        lines.push("target match 1".to_string());
        lines.push("target match 2".to_string());
        for _ in 0..1000 {
            lines.push("some other text".to_string());
        }
        lines.push("target match 3".to_string());

        let lines_ref: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        let (mut doc, path) = disk_doc("three-hits-test", &lines_ref);
        let query = crate::search::CompiledQuery::compile("target", false, true, '$').unwrap();

        // 1. 末尾のヒット（line 1002）の位置から「前へ」(forward = false) を呼ぶ -> line 1 (Hit 2) になること
        let found2 = doc.search_candidates(
            SearchSpec {
                query: &query,
                from: 1002,
                end: doc.line_count(),
                after_col: Some(0),
                forward: false,
            },
            &|| false,
        ).unwrap();
        assert_eq!(found2.hits.len(), 1);
        assert_eq!(found2.hits[0].line, 1, "Expected hit 2 (line 1), but got line {}", found2.hits[0].line);
        assert_eq!(found2.current_index, Some(2));
        assert_eq!(found2.total_matches, Some(3));

        // 2. さらに「前へ」-> line 0 (Hit 1) になること
        let found1 = doc.search_candidates(
            SearchSpec {
                query: &query,
                from: 1,
                end: doc.line_count(),
                after_col: Some(0),
                forward: false,
            },
            &|| false,
        ).unwrap();
        assert_eq!(found1.hits.len(), 1);
        assert_eq!(found1.hits[0].line, 0);
        assert_eq!(found1.current_index, Some(1));

        // 3. 先頭からさらに「前へ」-> 末尾の line 1002 (Hit 3) へラップアラウンドすること
        let found_wrap_back = doc.search_candidates(
            SearchSpec {
                query: &query,
                from: 0,
                end: doc.line_count(),
                after_col: Some(0),
                forward: false,
            },
            &|| false,
        ).unwrap();
        assert_eq!(found_wrap_back.hits.len(), 1);
        assert_eq!(found_wrap_back.hits[0].line, 1002);
        assert_eq!(found_wrap_back.current_index, Some(3));

        // 4. 末尾から「次へ」-> 先頭 line 0 (Hit 1) へラップアラウンドすること
        let found_wrap_fwd = doc.search_candidates(
            SearchSpec {
                query: &query,
                from: 1002,
                end: doc.line_count(),
                after_col: Some(6),
                forward: true,
            },
            &|| false,
        ).unwrap();
        assert!(!found_wrap_fwd.hits.is_empty());
        assert_eq!(found_wrap_fwd.hits[0].line, 0);
        assert_eq!(found_wrap_fwd.current_index, Some(1));

        std::fs::remove_file(path).ok();
    }

    #[test]
    fn test_unified_matcher_literal_and_regex() {
        let (mut doc, path) = crate::test_utils::disk_doc(
            "test_matcher",
            &["alpha 123", "beta 456", "gamma 123"],
        );

        // 1. LiteralMatcher
        let lit_query = super::CompiledQuery::compile("123", false, true, '\0').unwrap();
        let lit_found = doc
            .search_candidates(
                super::SearchSpec {
                    query: &lit_query,
                    from: 0,
                    end: doc.line_count(),
                    after_col: None,
                    forward: true,
                },
                &|| false,
            )
            .unwrap();
        assert_eq!(lit_found.hits.len(), 2);
        assert_eq!(lit_found.hits[0].line, 0);
        assert_eq!(lit_found.hits[0].start, 6);
        assert_eq!(lit_found.hits[0].end, 9);
        assert_eq!(lit_found.hits[1].line, 2);

        // 2. RegexMatcher
        let re_query = super::CompiledQuery::compile(r"\d{3}", true, true, '\0').unwrap();
        let re_found = doc
            .search_candidates(
                super::SearchSpec {
                    query: &re_query,
                    from: 0,
                    end: doc.line_count(),
                    after_col: None,
                    forward: true,
                },
                &|| false,
            )
            .unwrap();
        assert_eq!(re_found.hits.len(), 3); // 123, 456, 123
        assert_eq!(re_found.hits[0].line, 0);
        assert_eq!(re_found.hits[1].line, 1);
        assert_eq!(re_found.hits[2].line, 2);

        std::fs::remove_file(path).ok();
    }

    #[test]
    fn test_unified_pipeline_dirty_document_mapping() {
        let (mut doc, path) = crate::test_utils::disk_doc(
            "test_dirty",
            &["target 0", "target 1", "target 2", "target 3", "target 4"],
        );

        // 編集を実行:
        // 1. 行 2 ("target 2") を削除
        doc.replace(2, 3, Vec::new(), 1, "", "").unwrap();
        // 2. 行 1 の直前に "inserted target" を挿入
        doc.replace(1, 1, vec!["inserted target".to_string()], 2, "", "").unwrap();
        // 3. 末尾行 ("target 4") を "completely replaced" に置換
        let last_idx = doc.line_count() - 1; // 末尾行 ("target 4")
        doc.replace(last_idx, last_idx + 1, vec!["completely replaced".to_string()], 3, "", "").unwrap();

        // 検索実行 (Literal)
        let query = super::CompiledQuery::compile("target", false, true, '\0').unwrap();
        let found = doc
            .search_candidates(
                super::SearchSpec {
                    query: &query,
                    from: 0,
                    end: doc.line_count(),
                    after_col: None,
                    forward: true,
                },
                &|| false,
            )
            .unwrap();

        assert_eq!(found.total_matches, Some(4));
        assert_eq!(found.hits.len(), 4);
        assert_eq!(found.hits[0].line, 0);
        assert_eq!(found.hits[1].line, 1);
        assert_eq!(found.hits[2].line, 2);
        assert_eq!(found.hits[3].line, 3);

        // 二分探索ナビゲーション:
        // 行 2 (target 1) の位置から「前へ」-> 行 1 (inserted target, index 2)
        let prev = doc
            .search_candidates(
                super::SearchSpec {
                    query: &query,
                    from: 2,
                    end: doc.line_count(),
                    after_col: Some(0),
                    forward: false,
                },
                &|| false,
            )
            .unwrap();
        assert_eq!(prev.hits.len(), 1);
        assert_eq!(prev.hits[0].line, 1);
        assert_eq!(prev.current_index, Some(2));

        std::fs::remove_file(path).ok();
    }

    #[test]
    fn test_search_immediate_cancellation() {
        let lines = vec!["line with match"; 100];
        let (mut doc, path) = crate::test_utils::disk_doc("test_cancel", &lines);

        let query = super::CompiledQuery::compile("match", false, true, '\0').unwrap();
        let cancelled = doc
            .search_candidates(
                super::SearchSpec {
                    query: &query,
                    from: 0,
                    end: doc.line_count(),
                    after_col: None,
                    forward: true,
                },
                &|| true, // 即座にキャンセル
            )
            .unwrap();

        assert!(cancelled.cancelled);
        assert!(cancelled.hits.is_empty());

        std::fs::remove_file(path).ok();
    }
}


