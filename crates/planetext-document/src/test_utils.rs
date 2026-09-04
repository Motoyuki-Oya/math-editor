#![cfg(test)]

use crate::document::Document;
use crate::source::{FileEncoding, LineEnding};

/// 一意な一時ファイルに行を書き、開いた文書とパスを返す。
pub(crate) fn disk_doc(name: &str, lines: &[&str]) -> (Document, String) {
    let path = std::env::temp_dir().join(format!(
        "planetext-store-{}-{}.txt",
        std::process::id(),
        name
    ));
    let path = path.to_string_lossy().into_owned();
    std::fs::write(&path, lines.join("\n")).unwrap();
    let (mut doc, scan) = Document::open(&path).unwrap();
    if let Some(scan) = scan {
        scan.run().unwrap();
        doc.confirm_scan();
    }
    (doc, path)
}

pub(crate) fn all(doc: &mut Document) -> Vec<String> {
    doc.read(0, usize::MAX).unwrap()
}

pub(crate) fn utf16_file_bytes(text: &str, encoding: FileEncoding) -> Vec<u8> {
    let mut bytes = match encoding {
        FileEncoding::Utf16Le => b"\xFF\xFE".to_vec(),
        FileEncoding::Utf16Be => b"\xFE\xFF".to_vec(),
        _ => unreachable!(),
    };
    bytes.extend(text.encode_utf16().flat_map(|unit| match encoding {
        FileEncoding::Utf16Le => unit.to_le_bytes(),
        FileEncoding::Utf16Be => unit.to_be_bytes(),
        _ => unreachable!(),
    }));
    bytes
}

pub(crate) fn encoded_text(text: &str, encoding: FileEncoding) -> Vec<u8> {
    match encoding {
        FileEncoding::Utf16Le => text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
        FileEncoding::Utf16Be => text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
        _ => encoding.encode_str(text),
    }
}

pub(crate) fn encoded_disk_doc(
    name: &str,
    lines: &[&str],
    encoding: FileEncoding,
    line_ending: LineEnding,
    trailing_newline: bool,
) -> (Document, String) {
    let path =
        std::env::temp_dir().join(format!("planetext-store-{}-{name}.txt", std::process::id()));
    let separator = std::str::from_utf8(line_ending.as_bytes()).unwrap();
    let mut text = lines.join(separator);
    if trailing_newline {
        text.push_str(separator);
    }
    let bytes = match encoding {
        FileEncoding::Utf16Le | FileEncoding::Utf16Be => utf16_file_bytes(&text, encoding),
        _ => encoded_text(&text, encoding),
    };
    std::fs::write(&path, bytes).unwrap();
    let path = path.to_string_lossy().into_owned();
    let (doc, scan) = Document::open_with_encoding(&path, Some(encoding)).unwrap();
    assert!(scan.is_none());
    (doc, path)
}
pub(crate) fn assert_document_state(doc: &mut Document, expected: &[String]) {
    assert_eq!(all(doc), expected);
    assert_eq!(doc.line_count(), expected.len());
    assert_eq!(doc.pieces_line_count_for_test(), expected.len());
    assert_eq!(doc.pieces_newline_count_for_test(), expected.len().saturating_sub(1));
    let separator = std::str::from_utf8(doc.line_ending().as_bytes()).unwrap();
    assert_eq!(
        doc.bytes(),
        encoded_text(&expected.join(separator), doc.encoding()).len()
    );
    assert_eq!(doc.pieces_byte_len_for_test(), doc.bytes());
}

pub(crate) fn assert_encoded_document_state(doc: &mut Document, expected: &[&str]) {
    let expected_lines: Vec<String> = expected.iter().map(|line| (*line).to_string()).collect();
    let separator = std::str::from_utf8(doc.line_ending().as_bytes()).unwrap();
    let text = expected.join(separator);
    assert_eq!(all(doc), expected_lines);
    assert_eq!(doc.line_count(), expected.len());
    assert_eq!(doc.pieces_line_count_for_test(), expected.len());
    assert_eq!(doc.pieces_newline_count_for_test(), expected.len().saturating_sub(1));
    assert_eq!(doc.bytes(), encoded_text(&text, doc.encoding()).len());
    assert_eq!(doc.pieces_byte_len_for_test(), doc.bytes());
}
