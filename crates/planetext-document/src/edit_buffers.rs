use crate::source::{FileEncoding, LineEnding};

#[derive(Clone, Debug, Default)]
pub(crate) struct EditBuffers {
    insert: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct EditRange {
    pub(crate) from: usize,
    pub(crate) len: usize,
    pub(crate) lines: usize,
    pub(crate) encoding: FileEncoding,
    pub(crate) line_ending: LineEnding,
}

fn encode(text: &str, encoding: FileEncoding) -> Vec<u8> {
    match encoding {
        FileEncoding::Utf16Le => text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
        FileEncoding::Utf16Be => text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
        _ => encoding.encode_str(text),
    }
}
fn encoded_separator(encoding: FileEncoding, line_ending: LineEnding) -> Vec<u8> {
    encode(
        std::str::from_utf8(line_ending.as_bytes()).unwrap(),
        encoding,
    )
}

fn decode(bytes: &[u8], encoding: FileEncoding) -> String {
    match encoding {
        FileEncoding::Utf16Le => String::from_utf16_lossy(
            &bytes
                .chunks_exact(2)
                .map(|x| u16::from_le_bytes([x[0], x[1]]))
                .collect::<Vec<_>>(),
        ),
        FileEncoding::Utf16Be => String::from_utf16_lossy(
            &bytes
                .chunks_exact(2)
                .map(|x| u16::from_be_bytes([x[0], x[1]]))
                .collect::<Vec<_>>(),
        ),
        _ => encoding.decode_line(bytes),
    }
}

fn separator_at_or_after(
    bytes: &[u8],
    start: usize,
    separator: &[u8],
    encoding: FileEncoding,
) -> Option<usize> {
    let unit_bytes = match encoding {
        FileEncoding::Utf16Le | FileEncoding::Utf16Be => 2,
        _ => 1,
    };
    (start..=bytes.len().saturating_sub(separator.len()))
        .step_by(unit_bytes)
        .find(|&at| bytes[at..].starts_with(separator))
}

impl EditBuffers {
    pub(crate) fn append_lines(
        &mut self,
        lines: &[String],
        encoding: FileEncoding,
        line_ending: LineEnding,
    ) -> (EditRange, usize) {
        self.append_lines_with_boundaries(lines, encoding, line_ending, false, false)
    }

    pub(crate) fn append_lines_with_boundaries(
        &mut self,
        lines: &[String],
        encoding: FileEncoding,
        line_ending: LineEnding,
        starts_newline: bool,
        ends_newline: bool,
    ) -> (EditRange, usize) {
        let from = self.insert.len();
        let separator = encoded_separator(encoding, line_ending);
        if starts_newline {
            self.insert.extend_from_slice(&separator);
        }
        for (i, line) in lines.iter().enumerate() {
            if i != 0 {
                self.insert.extend_from_slice(&separator);
            }
            self.insert.extend_from_slice(&encode(line, encoding));
        }
        if ends_newline {
            self.insert.extend_from_slice(&separator);
        }
        (
            EditRange {
                from,
                len: self.insert.len() - from,
                lines: lines.len(),
                encoding,
                line_ending,
            },
            lines.len().saturating_sub(1) + usize::from(starts_newline) + usize::from(ends_newline),
        )
    }

    pub(crate) fn line_separator_len(encoding: FileEncoding, line_ending: LineEnding) -> usize {
        encoded_separator(encoding, line_ending).len()
    }

    pub(crate) fn for_each_line(
        &self,
        range: EditRange,
        skip: usize,
        take: usize,
        f: &mut dyn FnMut(usize, &str) -> bool,
    ) -> bool {
        if range.lines == 0 {
            return true;
        }
        let bytes = &self.insert[range.from..range.from + range.len];
        let separator = encoded_separator(range.encoding, range.line_ending);
        let mut start = 0;
        let mut line = 0;
        loop {
            let end = separator_at_or_after(bytes, start, &separator, range.encoding)
                .unwrap_or(bytes.len());
            if line >= skip && line < skip + take {
                let text = decode(&bytes[start..end], range.encoding);
                if !f(line - skip, &text) {
                    return false;
                }
            }
            line += 1;
            if end == bytes.len() {
                return true;
            }
            start = end + separator.len();
        }
    }

    pub(crate) fn bytes(&self, range: EditRange) -> &[u8] {
        &self.insert[range.from..range.from + range.len]
    }

    pub(crate) fn byte_offset_after_lines(&self, range: EditRange, lines: usize) -> usize {
        let bytes = &self.insert[range.from..range.from + range.len];
        let separator = encoded_separator(range.encoding, range.line_ending);
        let mut start = 0;
        for _ in 0..lines {
            if let Some(at) = separator_at_or_after(bytes, start, &separator, range.encoding) {
                start = at + separator.len();
            } else {
                return bytes.len();
            }
        }
        start
    }

    pub(crate) fn read_lines(&self, range: EditRange) -> Vec<String> {
        if range.lines == 0 {
            return Vec::new();
        }
        let bytes = &self.insert[range.from..range.from + range.len];
        let separator = encoded_separator(range.encoding, range.line_ending);
        let mut lines = Vec::with_capacity(range.lines);
        let mut start = 0;
        while let Some(end) = separator_at_or_after(bytes, start, &separator, range.encoding) {
            lines.push(decode(&bytes[start..end], range.encoding));
            start = end + separator.len();
        }
        lines.push(decode(&bytes[start..], range.encoding));
        lines.truncate(range.lines);
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn appended_ranges_are_independent_and_decode() {
        let mut buffers = EditBuffers::default();
        let (a, _) = buffers.append_lines(
            &["a".into(), "b".into()],
            FileEncoding::Utf8,
            LineEnding::Lf,
        );
        let (b, _) = buffers.append_lines(&["日本語".into()], FileEncoding::Utf8, LineEnding::CrLf);
        assert_eq!(buffers.read_lines(a), vec!["a", "b"]);
        assert_eq!(buffers.read_lines(b), vec!["日本語"]);
        let (utf16, _) = buffers.append_lines(
            &["左".into(), "右".into()],
            FileEncoding::Utf16Le,
            LineEnding::Cr,
        );
        assert_eq!(buffers.read_lines(utf16), vec!["左", "右"]);
    }

    #[test]
    fn empty_range_and_one_empty_line_remain_distinct() {
        let mut buffers = EditBuffers::default();
        let (empty, _) = buffers.append_lines(&[], FileEncoding::Utf8, LineEnding::CrLf);
        let (one_line, _) =
            buffers.append_lines(&[String::new()], FileEncoding::Utf8, LineEnding::CrLf);

        assert!(buffers.read_lines(empty).is_empty());
        assert_eq!(buffers.read_lines(one_line), vec![""]);
    }

    #[test]
    fn utf16_separator_search_stays_on_code_unit_boundaries() {
        for (encoding, line) in [
            (FileEncoding::Utf16Le, "\u{a01}\u{100}"),
            (FileEncoding::Utf16Be, "\u{100}\u{a01}"),
        ] {
            let mut buffers = EditBuffers::default();
            let expected = vec![line.to_string(), "tail".to_string()];
            let (range, _) = buffers.append_lines(&expected, encoding, LineEnding::CrLf);

            assert_eq!(buffers.read_lines(range), expected);
            assert_eq!(
                buffers.byte_offset_after_lines(range, 1),
                encode(line, encoding).len() + encoded_separator(encoding, LineEnding::CrLf).len()
            );
        }
    }
}
