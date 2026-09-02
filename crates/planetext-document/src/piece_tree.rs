const NODE_CAPACITY: usize = 8;
const MIN_OCCUPANCY: usize = NODE_CAPACITY / 2;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Piece {
    Source {
        from: usize,
        len: usize,
        newlines: usize,
        starts_newline: bool,
        ends_newline: bool,
    },
    Edit {
        from: usize,
        len: usize,
        newlines: usize,
        starts_newline: bool,
        ends_newline: bool,
        encoding: crate::source::FileEncoding,
        line_ending: crate::source::LineEnding,
    },
}
impl Piece {
    pub(crate) fn bytes(&self) -> usize {
        match self {
            Self::Source { len, .. } | Self::Edit { len, .. } => *len,
        }
    }
    pub(crate) fn newlines(&self) -> usize {
        match self {
            Self::Source { newlines, .. } | Self::Edit { newlines, .. } => *newlines,
        }
    }
    pub(crate) fn starts_newline(&self) -> bool {
        match self {
            Self::Source { starts_newline, .. } | Self::Edit { starts_newline, .. } => {
                *starts_newline
            }
        }
    }
    pub(crate) fn ends_newline(&self) -> bool {
        match self {
            Self::Source { ends_newline, .. } | Self::Edit { ends_newline, .. } => *ends_newline,
        }
    }
    pub(crate) fn lines(&self) -> usize {
        self.newlines() + usize::from(!self.ends_newline()) - usize::from(self.starts_newline())
    }
    fn split(&self, byte: usize, newlines: usize, left_ends_newline: bool) -> (Self, Self) {
        match self {
            Self::Source { from, len, .. } => (
                Self::Source {
                    from: *from,
                    len: byte,
                    newlines,
                    starts_newline: self.starts_newline(),
                    ends_newline: left_ends_newline,
                },
                Self::Source {
                    from: from + byte,
                    len: len - byte,
                    newlines: self.newlines() - newlines,
                    starts_newline: false,
                    ends_newline: self.ends_newline(),
                },
            ),
            Self::Edit {
                from,
                len,
                encoding,
                line_ending,
                ..
            } => (
                Self::Edit {
                    from: *from,
                    len: byte,
                    newlines,
                    starts_newline: self.starts_newline(),
                    ends_newline: left_ends_newline,
                    encoding: *encoding,
                    line_ending: *line_ending,
                },
                Self::Edit {
                    from: from + byte,
                    len: len - byte,
                    newlines: self.newlines() - newlines,
                    starts_newline: false,
                    ends_newline: self.ends_newline(),
                    encoding: *encoding,
                    line_ending: *line_ending,
                },
            ),
        }
    }
}

#[derive(Clone, Debug)]
enum NodeKind {
    Leaf(Vec<Piece>),
    Branch(Vec<Node>),
}
#[derive(Clone, Debug)]
struct Node {
    kind: NodeKind,
    bytes: usize,
    newlines: usize,
    pieces: usize,
    lines: usize,
    starts_newline: bool,
    ends_newline: bool,
}
impl Node {
    fn leaf(pieces: Vec<Piece>) -> Self {
        let mut n = Self {
            kind: NodeKind::Leaf(pieces),
            bytes: 0,
            newlines: 0,
            pieces: 0,
            lines: 0,
            starts_newline: false,
            ends_newline: false,
        };
        n.update();
        n
    }
    fn branch(children: Vec<Node>) -> Self {
        let mut n = Self {
            kind: NodeKind::Branch(children),
            bytes: 0,
            newlines: 0,
            pieces: 0,
            lines: 0,
            starts_newline: false,
            ends_newline: false,
        };
        n.update();
        n
    }
    fn update(&mut self) {
        match &self.kind {
            NodeKind::Leaf(p) => {
                self.bytes = p.iter().map(Piece::bytes).sum();
                self.newlines = p.iter().map(Piece::newlines).sum();
                self.pieces = p.len();
                self.starts_newline = p.first().is_some_and(Piece::starts_newline);
                self.ends_newline = p.last().is_some_and(Piece::ends_newline);
                self.lines = if p.is_empty() {
                    0
                } else {
                    self.newlines + usize::from(!self.ends_newline)
                        - usize::from(self.starts_newline)
                };
            }
            NodeKind::Branch(c) => {
                self.bytes = c.iter().map(|n| n.bytes).sum();
                self.newlines = c.iter().map(|n| n.newlines).sum();
                self.pieces = c.iter().map(|n| n.pieces).sum();
                self.starts_newline = c.first().is_some_and(|n| n.starts_newline);
                self.ends_newline = c.last().is_some_and(|n| n.ends_newline);
                self.lines = if c.is_empty() {
                    0
                } else {
                    self.newlines + usize::from(!self.ends_newline)
                        - usize::from(self.starts_newline)
                };
            }
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PieceTree {
    root: Node,
    pub(crate) byte_len: usize,
    pub(crate) newline_count: usize,
}
impl PieceTree {
    pub(crate) fn new(pieces: Vec<Piece>) -> Self {
        let mut t = Self {
            root: Node::leaf(Vec::new()),
            byte_len: 0,
            newline_count: 0,
        };
        for p in pieces {
            t.insert(t.root.pieces, p);
        }
        t
    }
    pub(crate) fn pieces(&self) -> Vec<Piece> {
        let mut out = Vec::new();
        collect(&self.root, &mut out);
        out
    }
    pub(crate) fn insert(&mut self, at: usize, piece: Piece) {
        let count = self.root.pieces;
        let split = insert_node(&mut self.root, at.min(count), piece);
        if let Some(right) = split {
            let left = std::mem::replace(&mut self.root, Node::leaf(Vec::new()));
            self.root = Node::branch(vec![left, right]);
        }
        self.sync();
    }
    pub(crate) fn remove(&mut self, from: usize, to: usize) {
        for _ in 0..to.saturating_sub(from) {
            if from >= self.root.pieces {
                break;
            }
            remove_node(&mut self.root, from);
        }
        collapse(&mut self.root);
        self.sync();
    }
    pub(crate) fn replace(&mut self, from: usize, to: usize, inserted: Vec<Piece>) {
        self.remove(from, to);
        for (i, p) in inserted.into_iter().enumerate() {
            self.insert(from + i, p);
        }
    }
    pub(crate) fn split_piece(
        &mut self,
        index: usize,
        byte: usize,
        newlines: usize,
        left_ends_newline: bool,
    ) {
        if let Some(p) = get_piece(&self.root, index).cloned() {
            if byte > 0 && byte <= p.bytes() {
                let (a, b) = p.split(byte, newlines, left_ends_newline);
                self.remove(index, index + 1);
                self.insert(index, b);
                self.insert(index, a);
            }
        }
    }

    pub(crate) fn source_range_index(&self, from: usize, len: usize) -> Option<usize> {
        find_source_range(&self.root, from, len, 0)
    }

    pub(crate) fn confirm_source_range(
        &mut self,
        from: usize,
        len: usize,
        newlines: usize,
    ) -> bool {
        let updated = update_source_range(&mut self.root, from, len, newlines);
        if updated {
            self.sync();
        }
        updated
    }

    pub(crate) fn trim_trailing_newline(&mut self, separator_len: usize) {
        let Some(index) = self.root.pieces.checked_sub(1) else {
            return;
        };
        let Some(mut piece) = get_piece(&self.root, index).cloned() else {
            return;
        };
        if !piece.ends_newline() || piece.bytes() < separator_len || piece.newlines() == 0 {
            return;
        }
        match &mut piece {
            Piece::Source {
                len,
                newlines,
                ends_newline,
                ..
            }
            | Piece::Edit {
                len,
                newlines,
                ends_newline,
                ..
            } => {
                *len -= separator_len;
                *newlines -= 1;
                *ends_newline = false;
            }
        }
        self.replace(index, index + 1, vec![piece]);
    }
    pub(crate) fn for_each_line_range(
        &self,
        from: usize,
        to: usize,
        f: &mut dyn FnMut(usize, Piece, usize, usize) -> bool,
    ) {
        visit(&self.root, from, to, 0, f);
    }
    pub(crate) fn locate_line(&self, line: usize) -> (usize, usize) {
        locate(&self.root, line, 0, 0)
    }
    pub(crate) fn piece(&self, index: usize) -> Option<&Piece> {
        get_piece(&self.root, index)
    }
    pub(crate) fn line_count(&self) -> usize {
        self.root.lines
    }
    pub(crate) fn byte_offset(&self, line: usize) -> usize {
        byte_at_line(&self.root, line.min(self.line_count()), 0)
    }
    pub(crate) fn byte_offset_of_piece(&self, piece_index: usize) -> usize {
        byte_of_piece(&self.root, piece_index, 0)
    }
    fn sync(&mut self) {
        self.byte_len = self.root.bytes;
        self.newline_count = self.root.newlines;
    }
}
fn collect(n: &Node, out: &mut Vec<Piece>) {
    match &n.kind {
        NodeKind::Leaf(p) => out.extend(p.iter().cloned()),
        NodeKind::Branch(c) => {
            for x in c {
                collect(x, out)
            }
        }
    }
}

fn find_source_range(node: &Node, from: usize, len: usize, piece_at: usize) -> Option<usize> {
    match &node.kind {
        NodeKind::Leaf(pieces) => {
            pieces
                .iter()
                .enumerate()
                .find_map(|(index, piece)| match piece {
                    Piece::Source {
                        from: piece_from,
                        len: piece_len,
                        ..
                    } if *piece_from == from && *piece_len == len => Some(piece_at + index),
                    _ => None,
                })
        }
        NodeKind::Branch(children) => {
            let mut child_at = piece_at;
            for child in children {
                if let Some(index) = find_source_range(child, from, len, child_at) {
                    return Some(index);
                }
                child_at += child.pieces;
            }
            None
        }
    }
}

fn update_source_range(node: &mut Node, from: usize, len: usize, exact_newlines: usize) -> bool {
    let updated = match &mut node.kind {
        NodeKind::Leaf(pieces) => pieces.iter_mut().any(|piece| match piece {
            Piece::Source {
                from: piece_from,
                len: piece_len,
                newlines,
                ends_newline,
                ..
            } if *piece_from == from && *piece_len == len => {
                *newlines = exact_newlines;
                *ends_newline = false;
                true
            }
            _ => false,
        }),
        NodeKind::Branch(children) => children
            .iter_mut()
            .any(|child| update_source_range(child, from, len, exact_newlines)),
    };
    if updated {
        node.update();
    }
    updated
}
fn insert_node(n: &mut Node, index: usize, piece: Piece) -> Option<Node> {
    let result = match &mut n.kind {
        NodeKind::Leaf(p) => {
            p.insert(index.min(p.len()), piece);
            if p.len() > NODE_CAPACITY {
                Some(Node::leaf(p.split_off(p.len() / 2)))
            } else {
                None
            }
        }
        NodeKind::Branch(c) => {
            let mut i = 0;
            let mut off = index;
            while i + 1 < c.len() && off > c[i].pieces {
                off -= c[i].pieces;
                i += 1;
            }
            let r = insert_node(&mut c[i], off, piece);
            if let Some(x) = r {
                c.insert(i + 1, x);
            }
            if c.len() > NODE_CAPACITY {
                Some(Node::branch(c.split_off(c.len() / 2)))
            } else {
                None
            }
        }
    };
    n.update();
    result
}
fn remove_node(n: &mut Node, index: usize) -> bool {
    let result = match &mut n.kind {
        NodeKind::Leaf(p) => {
            if index < p.len() {
                p.remove(index);
                true
            } else {
                false
            }
        }
        NodeKind::Branch(c) => {
            let mut i = 0;
            let mut off = index;
            while i < c.len() {
                if off < c[i].pieces {
                    let removed = remove_node(&mut c[i], off);
                    if removed {
                        rebalance_child(c, i);
                    }
                    return {
                        n.update();
                        removed
                    };
                }
                off -= c[i].pieces;
                i += 1;
            }
            false
        }
    };
    n.update();
    result
}
fn occupancy(n: &Node) -> usize {
    match &n.kind {
        NodeKind::Leaf(p) => p.len(),
        NodeKind::Branch(c) => c.len(),
    }
}
fn rebalance_child(children: &mut Vec<Node>, index: usize) {
    if occupancy(&children[index]) >= MIN_OCCUPANCY {
        return;
    }
    if index > 0 && occupancy(&children[index - 1]) > MIN_OCCUPANCY {
        let (left, rest) = children.split_at_mut(index);
        borrow_from_left(&mut rest[0], &mut left[index - 1]);
        return;
    }
    if index + 1 < children.len() && occupancy(&children[index + 1]) > MIN_OCCUPANCY {
        let (left, right) = children.split_at_mut(index + 1);
        borrow_from_right(&mut left[index], &mut right[0]);
        return;
    }
    if index > 0 {
        let right = children.remove(index);
        merge(&mut children[index - 1], right);
    } else if children.len() > 1 {
        let right = children.remove(1);
        merge(&mut children[0], right);
    }
}
fn borrow_from_left(node: &mut Node, left: &mut Node) {
    match (&mut node.kind, &mut left.kind) {
        (NodeKind::Leaf(to), NodeKind::Leaf(from)) => to.insert(0, from.pop().unwrap()),
        (NodeKind::Branch(to), NodeKind::Branch(from)) => to.insert(0, from.pop().unwrap()),
        _ => unreachable!("B-tree siblings always have the same kind"),
    }
    left.update();
    node.update();
}
fn borrow_from_right(node: &mut Node, right: &mut Node) {
    match (&mut node.kind, &mut right.kind) {
        (NodeKind::Leaf(to), NodeKind::Leaf(from)) => to.push(from.remove(0)),
        (NodeKind::Branch(to), NodeKind::Branch(from)) => to.push(from.remove(0)),
        _ => unreachable!("B-tree siblings always have the same kind"),
    }
    right.update();
    node.update();
}
fn merge(a: &mut Node, b: Node) {
    match (&mut a.kind, b.kind) {
        (NodeKind::Leaf(x), NodeKind::Leaf(mut y)) => x.append(&mut y),
        (NodeKind::Branch(x), NodeKind::Branch(mut y)) => x.append(&mut y),
        _ => unreachable!("B-tree siblings always have the same kind"),
    }
    a.update();
}
fn collapse(n: &mut Node) {
    if let NodeKind::Branch(c) = &mut n.kind {
        if c.len() == 1 {
            *n = c.remove(0);
            collapse(n);
        }
    }
    n.update();
}
fn get_piece(n: &Node, mut i: usize) -> Option<&Piece> {
    match &n.kind {
        NodeKind::Leaf(p) => p.get(i),
        NodeKind::Branch(c) => {
            for x in c {
                if i < x.pieces {
                    return get_piece(x, i);
                }
                i -= x.pieces;
            }
            None
        }
    }
}
fn visit(
    n: &Node,
    from: usize,
    to: usize,
    mut start: usize,
    f: &mut dyn FnMut(usize, Piece, usize, usize) -> bool,
) -> bool {
    if start >= to {
        return true;
    }
    match &n.kind {
        NodeKind::Leaf(p) => {
            for x in p {
                let end = start + x.lines();
                if end > from && start < to {
                    let skip = from.saturating_sub(start);
                    let take = to.min(end) - (start + skip);
                    if !f(start, x.clone(), skip, take) {
                        return false;
                    }
                }
                start = end;
            }
        }
        NodeKind::Branch(c) => {
            for x in c {
                if start + x.lines <= from {
                    start += x.lines;
                    continue;
                }
                if !visit(x, from, to, start, f) {
                    return false;
                }
                start += x.lines;
            }
        }
    }
    true
}
fn locate(n: &Node, line: usize, mut pi: usize, mut start: usize) -> (usize, usize) {
    match &n.kind {
        NodeKind::Leaf(p) => {
            for x in p {
                let end = start + x.lines();
                if line < end {
                    return (pi, start);
                }
                start = end;
                pi += 1;
            }
            (pi, start)
        }
        NodeKind::Branch(c) => {
            for x in c {
                let end = start + x.lines;
                if line < end {
                    return locate(x, line, pi, start);
                }
                start = end;
                pi += x.pieces;
            }
            (pi, start)
        }
    }
}

fn byte_at_line(node: &Node, mut line: usize, mut bytes: usize) -> usize {
    match &node.kind {
        NodeKind::Leaf(pieces) => {
            for piece in pieces {
                if line < piece.lines() {
                    return bytes;
                }
                line -= piece.lines();
                bytes += piece.bytes();
            }
            bytes
        }
        NodeKind::Branch(children) => {
            for child in children {
                if line < child.lines {
                    return byte_at_line(child, line, bytes);
                }
                line -= child.lines;
                bytes += child.bytes;
            }
            bytes
        }
    }
}

fn byte_of_piece(node: &Node, mut index: usize, mut bytes: usize) -> usize {
    match &node.kind {
        NodeKind::Leaf(pieces) => {
            for piece in pieces {
                if index == 0 {
                    return bytes;
                }
                index -= 1;
                bytes += piece.bytes();
            }
            bytes
        }
        NodeKind::Branch(children) => {
            for child in children {
                if index < child.pieces {
                    return byte_of_piece(child, index, bytes);
                }
                index -= child.pieces;
                bytes += child.bytes;
            }
            bytes
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn p(len: usize, nl: usize) -> Piece {
        Piece::Source {
            from: 0,
            len,
            newlines: nl,
            starts_newline: false,
            ends_newline: false,
        }
    }
    #[test]
    fn ordering_and_aggregates_survive_rebalancing() {
        let mut t = PieceTree::new(Vec::new());
        for i in 0..100 {
            t.insert(i, p(i + 1, i % 3));
        }
        assert_eq!(t.pieces().len(), 100);
        assert_eq!(t.byte_len, (1..=100).sum());
        assert_eq!(t.newline_count, (0..100).map(|i| i % 3).sum());
        t.remove(1, 99);
        assert_eq!(t.pieces().len(), 2);
    }
    #[test]
    fn deletion_keeps_order_and_aggregates() {
        let mut t = PieceTree::new((0..40).map(|i| p(i + 1, 1)).collect());
        t.remove(7, 31);
        assert_eq!(t.pieces().len(), 16);
        assert_eq!(t.newline_count, 16);
    }
    #[test]
    fn split_metadata_preserves_lengths() {
        let x = p(10, 2);
        let (a, b) = x.split(5, 1, true);
        assert_eq!((a.bytes(), a.newlines(), a.lines()), (5, 1, 1));
        assert_eq!((b.bytes(), b.newlines(), b.lines()), (5, 1, 2));
    }

    #[test]
    fn splitting_after_a_trailing_newline_keeps_the_empty_right_line() {
        let mut tree = PieceTree::new(vec![p(5, 1)]);
        tree.split_piece(0, 5, 1, true);

        let pieces = tree.pieces();
        assert_eq!(pieces.len(), 2);
        assert_eq!((pieces[0].bytes(), pieces[0].lines()), (5, 1));
        assert_eq!((pieces[1].bytes(), pieces[1].lines()), (0, 1));
        assert_eq!(tree.byte_len, 5);
        assert_eq!(tree.newline_count, 1);
        assert_eq!(tree.line_count(), 2);
        assert_invariants(&tree.root, true, 0, &mut None);
    }

    fn assert_invariants(
        node: &Node,
        is_root: bool,
        depth: usize,
        leaf_depth: &mut Option<usize>,
    ) -> Vec<Piece> {
        let occupancy = occupancy(node);
        assert!(occupancy <= NODE_CAPACITY);
        if !is_root {
            assert!(occupancy >= MIN_OCCUPANCY);
        }
        let pieces = match &node.kind {
            NodeKind::Leaf(pieces) => {
                assert_eq!(*leaf_depth.get_or_insert(depth), depth);
                pieces.clone()
            }
            NodeKind::Branch(children) => {
                if is_root {
                    assert!(children.len() >= 2);
                }
                let mut pieces = Vec::new();
                for child in children {
                    pieces.extend(assert_invariants(child, false, depth + 1, leaf_depth));
                }
                pieces
            }
        };
        assert_eq!(node.bytes, pieces.iter().map(Piece::bytes).sum::<usize>());
        assert_eq!(
            node.newlines,
            pieces.iter().map(Piece::newlines).sum::<usize>()
        );
        assert_eq!(node.pieces, pieces.len());
        assert_eq!(
            node.lines,
            if pieces.is_empty() {
                0
            } else {
                node.newlines + usize::from(!node.ends_newline) - usize::from(node.starts_newline)
            }
        );
        pieces
    }

    #[test]
    fn mixed_edits_keep_btree_occupancy_depth_order_and_aggregates() {
        let mut tree = PieceTree::new(Vec::new());
        let mut expected = Vec::new();
        for id in 1..=240 {
            let at = (id * 37) % (expected.len() + 1);
            let piece = Piece::Source {
                from: id,
                len: id % 17 + 1,
                newlines: id % 4,
                starts_newline: false,
                ends_newline: false,
            };
            tree.insert(at, piece.clone());
            expected.insert(at, piece);
        }
        for step in 0..170 {
            let at = (step * 29 + 11) % expected.len();
            tree.remove(at, at + 1);
            expected.remove(at);
            if step % 3 == 0 {
                let id = 1_000 + step;
                let at = (step * 13) % (expected.len() + 1);
                let piece = Piece::Source {
                    from: id,
                    len: id % 19 + 1,
                    newlines: id % 5,
                    starts_newline: false,
                    ends_newline: false,
                };
                tree.insert(at, piece.clone());
                expected.insert(at, piece);
            }
            let actual = assert_invariants(&tree.root, true, 0, &mut None);
            assert_eq!(actual, expected);
            assert_eq!(tree.byte_len, tree.root.bytes);
            assert_eq!(tree.newline_count, tree.root.newlines);
        }
    }

    #[test]
    fn line_ranges_report_global_piece_line_starts() {
        let pieces = vec![
            Piece::Source {
                from: 0,
                len: 4,
                newlines: 2,
                starts_newline: false,
                ends_newline: true,
            },
            Piece::Edit {
                from: 0,
                len: 6,
                newlines: 3,
                starts_newline: false,
                ends_newline: true,
                encoding: crate::source::FileEncoding::Utf8,
                line_ending: crate::source::LineEnding::Lf,
            },
            p(2, 0),
        ];
        let tree = PieceTree::new(pieces);
        let mut visited = Vec::new();
        tree.for_each_line_range(1, 6, &mut |start, _, skip, take| {
            visited.push((start, skip, take));
            true
        });
        assert_eq!(visited, vec![(0, 1, 1), (2, 0, 3), (5, 0, 1)]);
    }
}
