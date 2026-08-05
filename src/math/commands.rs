//! `\name` shortcuts. The same table serves the text editor and the formula
//! editor, so `\sqrt` behaves identically wherever it is typed.

use super::ast::{self, MatrixKind, Node};
use super::symbols;

/// The node a `\name` shortcut expands to, if the name is known.
pub fn node_for(name: &str) -> Option<Node> {
    match name {
        "frac" => Some(ast::frac()),
        "sqrt" => Some(ast::sqrt()),
        "root" | "nthroot" => Some(ast::nth_root()),
        "matrix" => Some(ast::matrix(MatrixKind::Plain, 2, 2)),
        "pmatrix" => Some(ast::matrix(MatrixKind::Paren, 2, 2)),
        "bmatrix" => Some(ast::matrix(MatrixKind::Bracket, 2, 2)),
        "cases" => Some(ast::matrix(MatrixKind::Cases, 2, 2)),
        _ if symbols::big_op(name).is_some() => Some(ast::big_op(name)),
        _ if symbols::is_function(name) => Some(Node::Func(name.to_string())),
        _ if symbols::lookup(name).is_some() => Some(Node::Sym(name.to_string())),
        _ => None,
    }
}

pub fn is_command(name: &str) -> bool {
    node_for(name).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structures_symbols_and_functions_are_known() {
        assert!(matches!(node_for("sqrt"), Some(Node::Sqrt { .. })));
        assert!(matches!(node_for("sum"), Some(Node::BigOp { .. })));
        assert!(matches!(node_for("cases"), Some(Node::Matrix { .. })));
        assert!(matches!(node_for("alpha"), Some(Node::Sym(_))));
        assert!(matches!(node_for("sin"), Some(Node::Func(_))));
    }

    #[test]
    fn unknown_names_are_rejected() {
        assert!(node_for("notacommand").is_none());
    }
}
