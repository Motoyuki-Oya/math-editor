//! `\name` shortcuts. The same table serves the text editor and the island
//! editor, so `\sqrt` behaves identically wherever it is typed.

use super::ast::{self, MatrixKind, Node};
use super::symbols;

/// The node a `\name` shortcut expands to, if the name is known.
pub fn node_for(name: &str) -> Option<Node> {
    match name {
        "stack" | "frac" => Some(ast::stack(true)),
        "atop" => Some(ast::stack(false)),
        "sqrt" => Some(ast::sqrt()),
        "root" | "nthroot" => Some(ast::nth_root()),
        "matrix" => Some(ast::matrix(MatrixKind::Grid, 2, 2)),
        "cases" => Some(ast::matrix(MatrixKind::Cases, 2, 2)),
        _ if symbols::big_op(name).is_some() => Some(limits_for(name)),
        _ if symbols::is_function(name) => Some(Node::Func(name.to_string())),
        _ if symbols::lookup(name).is_some() => Some(Node::Sym(name.to_string())),
        _ => None,
    }
}

/// A symbol with room above and below it, using the glyph a name stands for.
fn limits_for(name: &str) -> Node {
    let glyph = symbols::big_op(name).map(|op| op.glyph).unwrap_or(name);
    ast::limits(glyph)
}

/// The structure a directly typed glyph expands to, so `√` works like
/// `\sqrt`. Glyphs that need no structure (`α`, `×`) stay plain text.
pub fn node_for_glyph(glyph: char) -> Option<Node> {
    if glyph == '√' {
        return Some(ast::sqrt());
    }
    let text = glyph.to_string();
    symbols::BIG_OPS
        .iter()
        .find(|op| op.glyph == text)
        .map(|op| ast::limits(op.glyph))
}

/// The glyph a symbol name prints as, for inserting it as plain text.
pub fn glyph_for(name: &str) -> Option<&'static str> {
    symbols::lookup(name).map(|symbol| symbol.glyph)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structures_symbols_and_functions_are_known() {
        assert!(matches!(node_for("sqrt"), Some(Node::Sqrt { .. })));
        assert!(matches!(node_for("sum"), Some(Node::Limits { .. })));
        assert!(matches!(node_for("cases"), Some(Node::Matrix { .. })));
        assert!(matches!(node_for("alpha"), Some(Node::Sym(_))));
        assert!(matches!(node_for("sin"), Some(Node::Func(_))));
    }

    #[test]
    fn unknown_names_are_rejected() {
        assert!(node_for("notacommand").is_none());
    }

    #[test]
    fn only_structural_glyphs_expand() {
        assert!(matches!(node_for_glyph('√'), Some(Node::Sqrt { .. })));
        assert!(matches!(node_for_glyph('∑'), Some(Node::Limits { .. })));
        assert!(node_for_glyph('α').is_none());
        assert!(node_for_glyph('a').is_none());
    }

    #[test]
    fn symbol_names_have_glyphs() {
        assert_eq!(glyph_for("alpha"), Some("α"));
        assert_eq!(glyph_for("nope"), None);
    }
}
