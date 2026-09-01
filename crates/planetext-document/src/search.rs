use crate::document::Document;
use crate::edit_buffers::EditRange;
use crate::piece_tree::Piece;
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

#[derive(Clone, Copy)]
struct RawScanHit {
    line: usize,
    notation: bool,
    line_start: usize,
    start: usize,
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

fn convert_raw_hits(
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
fn scan_encoded_range(
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
    let query_has_delimiter = aligned_positions(query, delimiter, true, unit)
        .first()
        .is_some();
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
        let mut events = Vec::new();
        for at in aligned_positions(&carry, delimiter, true, unit) {
            let absolute = data_start + at;
            if absolute >= processed && absolute < safe_end {
                events.push((absolute, 2_u8));
            }
        }
        for at in aligned_positions(&carry, marker, true, unit) {
            let absolute = data_start + at;
            if absolute >= processed && absolute < safe_end {
                events.push((absolute, 1_u8));
            }
        }
        if !query_has_delimiter {
            for at in aligned_positions(&carry, query, case_sensitive, unit) {
                let absolute = data_start + at;
                if absolute >= processed && absolute < safe_end && absolute >= next_literal_end {
                    events.push((absolute, 0_u8));
                }
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
#[derive(serde::Serialize)]
pub(crate) struct ScanHit {
    pub(crate) line: usize,
    pub(crate) notation: bool,
    /// 行内の一致の文字位置。`notation` の行では意味を持たない。
    pub(crate) start: usize,
    pub(crate) end: usize,
}

pub(crate) struct SearchCandidates {
    pub(crate) hits: Vec<ScanHit>,
    pub(crate) scanned_to: usize,
    pub(crate) cancelled: bool,
}

pub(crate) struct SearchSpec<'a> {
    pub(crate) pattern: &'a regex::Regex,
    pub(crate) literal: Option<&'a str>,
    pub(crate) case_sensitive: bool,
    pub(crate) marker: char,
    pub(crate) from: usize,
    pub(crate) end: usize,
    pub(crate) after_col: Option<usize>,
}

impl Document {
    /// `from..end` を native 内で連続走査し、最初の候補群まで進む。空の
    /// ページを frontend と往復せず、ページごとにキャンセルを確認する。
    pub(crate) fn search_candidates(
        &mut self,
        spec: SearchSpec<'_>,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<SearchCandidates, String> {
        // 1回の読みを大きくしてseek・確保を減らしつつ、キャンセル確認は
        let mut page_lines = 10_000;
        const MAX_PAGE_LINES: usize = 1_000_000;
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
            let page_end = (at + page_lines).min(end);
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
            page_lines = (page_lines * 4).min(MAX_PAGE_LINES);
        }
        Ok(SearchCandidates {
            hits: Vec::new(),
            scanned_to: end,
            cancelled: false,
        })
    }

    /// 通常の大小区別あり文字列検索。ディスクのピースはバイト範囲をまとめて
    /// memmem で探し、編集で入った行も同じ結果形式へ合わせる。
    pub(crate) fn scan_literal(
        &mut self,
        query: &str,
        case_sensitive: bool,
        marker: char,
        from: usize,
        count: usize,
        limit: usize,
    ) -> Result<(Vec<ScanHit>, usize), String> {
        let to = from.saturating_add(count).min(self.count);
        if from >= to || limit == 0 {
            return Ok((Vec::new(), from));
        }
        let query_characters = query.chars().count();
        let mut hits = Vec::new();
        let mut scanned_to = from;
        let mut error = None;
        self.pieces
            .for_each_line_range(from, to, &mut |piece_line, piece, skip, take| {
                if hits.len() >= limit || error.is_some() {
                    return false;
                }
                let result: Result<(Vec<ScanHit>, usize), String> = match piece {
                    Piece::Source { from, len, .. } => {
                        let Some(source) = self.source.as_mut() else {
                            error = Some(
                                "文書ストアのディスク参照が失われました。開き直してください"
                                    .to_string(),
                            );
                            return false;
                        };
                        (|| {
                            let (range_from, range_to) =
                                source.byte_range_for_lines(from, len, skip, take)?;
                            let encoding = source.encoding;
                            let delimiter = source.delimiter();
                            let encoded_query = encoding.encode_str(query);
                            let encoded_marker = encoding.encode_str(&marker.to_string());
                            let (raw, scanned) = scan_encoded_range(
                                range_to - range_from,
                                take,
                                encoding,
                                &delimiter,
                                &encoded_query,
                                &encoded_marker,
                                case_sensitive,
                                limit - hits.len(),
                                |offset, size| source.read_byte_range(range_from + offset, size),
                            )?;
                            let converted = convert_raw_hits(
                                raw,
                                encoding,
                                piece_line + skip,
                                query_characters,
                                |offset, size| source.read_byte_range(range_from + offset, size),
                            )?;
                            Ok((converted, scanned))
                        })()
                    }
                    Piece::Edit {
                        from,
                        len,
                        newlines,
                        starts_newline,
                        ends_newline,
                        encoding,
                        line_ending,
                    } => {
                        let leading = usize::from(starts_newline)
                            * crate::edit_buffers::EditBuffers::line_separator_len(
                                encoding,
                                line_ending,
                            );
                        let range = EditRange {
                            from: from + leading,
                            len: len - leading,
                            lines: newlines + usize::from(!ends_newline)
                                - usize::from(starts_newline),
                            encoding,
                            line_ending,
                        };
                        let bytes = self.buffers.bytes(range);
                        let range_start = self.buffers.byte_offset_after_lines(range, skip);
                        let range_end = self
                            .buffers
                            .byte_offset_after_lines(range, skip.saturating_add(take));
                        let selected = &bytes[range_start..range_end];
                        let delimiter = encoding.encode_str(match line_ending {
                            crate::source::LineEnding::Cr => "\r",
                            _ => "\n",
                        });
                        let encoded_query = encoding.encode_str(query);
                        let encoded_marker = encoding.encode_str(&marker.to_string());
                        scan_encoded_range(
                            selected.len(),
                            take,
                            encoding,
                            &delimiter,
                            &encoded_query,
                            &encoded_marker,
                            case_sensitive,
                            limit - hits.len(),
                            |offset, size| Ok(selected[offset..offset + size].to_vec()),
                        )
                        .and_then(|(raw, scanned)| {
                            let converted = convert_raw_hits(
                                raw,
                                encoding,
                                piece_line + skip,
                                query_characters,
                                |offset, size| Ok(selected[offset..offset + size].to_vec()),
                            )?;
                            Ok((converted, scanned))
                        })
                    }
                };
                match result {
                    Ok((mut piece_hits, scanned)) => {
                        hits.append(&mut piece_hits);
                        scanned_to = piece_line + skip + scanned;
                        hits.len() < limit
                    }
                    Err(message) => {
                        error = Some(message);
                        false
                    }
                }
            });
        if let Some(error) = error {
            Err(error)
        } else {
            Ok((hits, scanned_to))
        }
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
                false
            } else {
                true
            }
        })?;
        Ok((hits, scanned_to))
    }

    /// 文書から等間隔の窓を標本として検索し、全文の一致数を推定する。
    /// 小さい文書は全行を調べるので正確な件数になる。
    pub(crate) fn estimate_matches(&mut self, pattern: &regex::Regex) -> Result<usize, String> {
        const WINDOWS: usize = 64;
        const LINES_PER_WINDOW: usize = 2_000;
        let step = self.count.div_ceil(WINDOWS).max(1);
        let take = LINES_PER_WINDOW.min(step);
        let mut hits = 0;
        let mut sampled = 0;
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
        // 切り上げ丸めは全行ヒット時に count+1 を返してしまうので、切り捨てで推定する。
        Ok(((hits as u128 * self.count as u128 / sampled as u128) as usize).min(self.count))
    }

    /// `from..=to` の行のうち `needle` を含むもの。
    pub(crate) fn lines_containing(
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
}
