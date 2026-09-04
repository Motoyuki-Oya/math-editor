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

#[derive(Clone, Copy)]
pub(crate) struct RawScanHit {
    pub(crate) line: usize,
    pub(crate) notation: bool,
    pub(crate) line_start: usize,
    pub(crate) start: usize,
}

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
#[derive(serde::Serialize, Debug, Clone)]
pub struct ScanHit {
    pub line: usize,
    pub notation: bool,
    /// 行内の一致の文字位置。`notation` の行では意味を持たない。
    pub start: usize,
    pub end: usize,
}

use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct CompiledQuery {
    pub pattern: Arc<regex::Regex>,
    pub literal: Option<String>,
    pub case_sensitive: bool,
    pub marker: char,
}

impl CompiledQuery {
    pub fn compile(
        query: &str,
        regex: bool,
        case_sensitive: bool,
        marker: char,
    ) -> Result<Self, String> {
        let pattern_str = if regex {
            query.to_string()
        } else {
            regex::escape(query)
        };
        let pattern = regex::RegexBuilder::new(&pattern_str)
            .case_insensitive(!case_sensitive)
            .build()
            .map_err(|e| format!("正規表現を読めませんでした: {e}"))?;
        let literal = (!regex && (case_sensitive || query.is_ascii())).then(|| query.to_string());
        Ok(Self {
            pattern: Arc::new(pattern),
            literal,
            case_sensitive,
            marker,
        })
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
}

impl Document {
    /// `from..end` を native 内で走査し、候補群を返す。
    /// 未走査の場合はドキュメント全体を一気にゼロコピー走査して全ヒットをキャッシュに保存し、
    /// 正確な件数（total_matches）と現在何件目か（current_index）を即座に特定する。
    pub(crate) fn search_candidates(
        &mut self,
        spec: SearchSpec<'_>,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<SearchCandidates, String> {
        let pattern = &spec.query.pattern;
        let literal = spec.query.literal.as_deref();
        let case_sensitive = spec.query.case_sensitive;
        let marker = spec.query.marker;
        let from = spec.from;
        let end = spec.end;
        let after_col = spec.after_col;
        let cur_rev = self.revision();
        let pattern_str = pattern.as_str();

        let is_cached = if let Some(cache) = &self.search_cache {
            cache.revision == cur_rev
                && cache.case_sensitive == case_sensitive
                && cache.pattern == pattern_str
                && cache.literal.as_deref() == literal
                && cache.fully_scanned
        } else {
            false
        };

        if !is_cached {
            let doc_lines = self.line_count();
            let mut all_hits = Vec::new();
            let mut at = 0;
            let mut page_lines = 50_000;
            const MAX_PAGE_LINES: usize = 1_000_000;
            let is_clean = self.is_clean();
            let mod_lines = if !is_clean {
                self.modified_lines()
            } else {
                Vec::new()
            };

            while at < doc_lines {
                if cancelled() {
                    return Ok(SearchCandidates {
                        hits: Vec::new(),
                        scanned_to: at,
                        cancelled: true,
                        total_matches: None,
                        current_index: None,
                    });
                }

                let page_end = (at + page_lines).min(doc_lines);
                let skip_scan = if case_sensitive {
                    if let (Some(index), Some(query)) = (self.search_index().cloned(), literal) {
                        if let (Ok(start_byte), Ok(end_byte)) = (
                            self.byte_offset_of_line(at),
                            self.byte_offset_of_line(page_end),
                        ) {
                            let start_block = start_byte / crate::search_index::INDEX_BLOCK_BYTES;
                            let end_block = end_byte / crate::search_index::INDEX_BLOCK_BYTES;
                            let is_clean_range = if is_clean {
                                true
                            } else {
                                !mod_lines.iter().any(|&line| line >= at && line < page_end)
                            };
                            if is_clean_range {
                                (start_block..=end_block).all(|b| !index.may_contain_query(b, query))
                            } else {
                                false
                            }
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                } else {
                    false
                };

                if skip_scan {
                    at = page_end;
                    page_lines = (page_lines * 4).min(MAX_PAGE_LINES);
                    continue;
                }

                let (hits, _) = if let Some(query) = literal {
                    self.scan_literal(query, case_sensitive, marker, at, page_end - at, usize::MAX)?
                } else {
                    self.scan(pattern, marker, at, page_end - at, usize::MAX)?
                };
                all_hits.extend(hits);
                at = page_end;
                page_lines = (page_lines * 4).min(MAX_PAGE_LINES);
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
        let total_matches = Some(cache.hits.len());

        let mut matching_hits = Vec::new();
        let mut first_match_idx = None;

        for (idx, hit) in cache.hits.iter().enumerate() {
            if hit.line < from || hit.line >= end {
                continue;
            }
            if hit.line == from {
                if let Some(col) = after_col {
                    if !hit.notation && hit.start < col {
                        continue;
                    }
                }
            }
            if first_match_idx.is_none() {
                first_match_idx = Some(idx + 1); // 1-indexed
            }
            matching_hits.push(hit.clone());
            if matching_hits.len() >= 64 {
                break;
            }
        }

        let scanned_to = matching_hits.last().map_or(end, |hit| hit.line + 1);
        Ok(SearchCandidates {
            hits: matching_hits,
            scanned_to,
            cancelled: false,
            total_matches,
            current_index: first_match_idx,
        })
    }

    /// 検索の走査で見つかったもの: 素の行の一致か、読み替え（記法の解釈）を
    /// 要する行。行の順に並ぶ。
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

    pub(crate) fn estimate_matches(&mut self, pattern: &regex::Regex) -> Result<usize, String> {
        self.confirm_scan_if_done();
        let cur_rev = self.revision();
        let pattern_str = pattern.as_str();

        if let Some(cache) = &self.search_cache {
            if cache.fully_scanned && cache.revision == cur_rev && cache.pattern == pattern_str {
                return Ok(cache.hits.len());
            }
        }

        if let Some(index) = self.search_index() {
            let query = pattern.as_str();
            let is_literal = !query.contains([
                '\\', '.', '+', '*', '?', '(', ')', '|', '[', ']', '{', '}', '^', '$',
            ]);
            if is_literal {
                if let Some(estimated) = index.estimate_matches(query) {
                    return Ok(estimated);
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
                hits += pattern.find_iter(line).count();
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

}
