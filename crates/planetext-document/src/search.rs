use crate::document::Document;

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
        let mut hits = Vec::new();
        self.each_line(from, to - from, &mut |at, text| {
            if hits.len() >= limit {
                return false;
            }
            if text.contains(marker) {
                hits.push(ScanHit {
                    line: at,
                    notation: true,
                    start: 0,
                    end: 0,
                });
            } else {
                for byte in literal_positions(text.as_bytes(), query.as_bytes(), case_sensitive) {
                    let start_col = text[..byte].chars().count();
                    hits.push(ScanHit {
                        line: at,
                        notation: false,
                        start: start_col,
                        end: start_col + query.chars().count(),
                    });
                    if hits.len() >= limit {
                        break;
                    }
                }
            }
            true
        })?;
        let scanned_to = to;
        return Ok((hits, scanned_to));
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
