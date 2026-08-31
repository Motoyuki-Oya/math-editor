use crate::edit_buffers::EditBuffer;

/// 文書のひと続き: ディスクにそのまま残っている行の範囲か、編集で入った行。
#[derive(Clone, Debug)]
pub(crate) enum Piece {
    Disk { from: usize, lines: usize },
    Fresh(EditBuffer),
}

impl Piece {
    pub(crate) fn len(&self) -> usize {
        match self {
            Piece::Disk { lines, .. } => *lines,
            Piece::Fresh(lines) => lines.len(),
        }
    }
}
