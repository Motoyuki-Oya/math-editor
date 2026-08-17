//! 語彙: 編集者が知っているすべての名前とグリフ、およびそれぞれが何を表すか。ショートカットは同じ語彙からの名前にすぎないため、シンボル テーブルと `\name` ショートカットは一緒に存在します。同じテーブルがテキスト エディタとアイランド エディタに機能するため、`\sqrt` はどこに入力しても同じように動作します。

use super::ast::{self, Between, MatrixKind, Node};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// 変数のようなもの、横にあるものに対してぴったりと描画されます。
    Ident,
    /// 二項演算子、両側にスペースをあけて描画されます。
    Bin,
    /// 関係、両側に広いスペースを使って描画されます。
    Rel,
    /// 句読点とその他すべて、余分なスペースを入れずに直立して描画されます。
    Plain,
}

pub struct Symbol {
    pub name: &'static str,
    pub glyph: &'static str,
    pub class: Class,
}

macro_rules! symbols {
    ($(($name:literal, $glyph:literal, $class:ident)),* $(,)?) => {
        pub const SYMBOLS: &[Symbol] = &[
            $(Symbol { name: $name, glyph: $glyph, class: Class::$class }),*
        ];
    };
}

symbols![
    ("alpha", "α", Ident),
    ("beta", "β", Ident),
    ("gamma", "γ", Ident),
    ("delta", "δ", Ident),
    ("epsilon", "ε", Ident),
    ("varepsilon", "ε", Ident),
    ("zeta", "ζ", Ident),
    ("eta", "η", Ident),
    ("theta", "θ", Ident),
    ("vartheta", "ϑ", Ident),
    ("iota", "ι", Ident),
    ("kappa", "κ", Ident),
    ("lambda", "λ", Ident),
    ("mu", "μ", Ident),
    ("nu", "ν", Ident),
    ("xi", "ξ", Ident),
    ("pi", "π", Ident),
    ("rho", "ρ", Ident),
    ("sigma", "σ", Ident),
    ("tau", "τ", Ident),
    ("upsilon", "υ", Ident),
    ("phi", "φ", Ident),
    ("varphi", "ϕ", Ident),
    ("chi", "χ", Ident),
    ("psi", "ψ", Ident),
    ("omega", "ω", Ident),
    ("Gamma", "Γ", Ident),
    ("Delta", "Δ", Ident),
    ("Theta", "Θ", Ident),
    ("Lambda", "Λ", Ident),
    ("Xi", "Ξ", Ident),
    ("Pi", "Π", Ident),
    ("Sigma", "Σ", Ident),
    ("Phi", "Φ", Ident),
    ("Psi", "Ψ", Ident),
    ("Omega", "Ω", Ident),
    ("infty", "∞", Plain),
    ("partial", "∂", Ident),
    ("nabla", "∇", Ident),
    ("hbar", "ℏ", Ident),
    ("ell", "ℓ", Ident),
    ("Re", "ℜ", Ident),
    ("Im", "ℑ", Ident),
    ("aleph", "ℵ", Ident),
    ("times", "×", Bin),
    ("div", "÷", Bin),
    ("cdot", "⋅", Bin),
    ("pm", "±", Bin),
    ("mp", "∓", Bin),
    ("ast", "∗", Bin),
    ("star", "⋆", Bin),
    ("circ", "∘", Bin),
    ("oplus", "⊕", Bin),
    ("otimes", "⊗", Bin),
    ("cup", "∪", Bin),
    ("cap", "∩", Bin),
    ("setminus", "∖", Bin),
    ("leq", "≤", Rel),
    ("le", "≤", Rel),
    ("geq", "≥", Rel),
    ("ge", "≥", Rel),
    ("neq", "≠", Rel),
    ("ne", "≠", Rel),
    ("approx", "≈", Rel),
    ("sim", "∼", Rel),
    ("simeq", "≃", Rel),
    ("equiv", "≡", Rel),
    ("propto", "∝", Rel),
    ("ll", "≪", Rel),
    ("gg", "≫", Rel),
    ("subset", "⊂", Rel),
    ("subseteq", "⊆", Rel),
    ("supset", "⊃", Rel),
    ("supseteq", "⊇", Rel),
    ("in", "∈", Rel),
    ("notin", "∉", Rel),
    ("ni", "∋", Rel),
    ("perp", "⊥", Rel),
    ("parallel", "∥", Rel),
    ("mid", "∣", Rel),
    ("to", "→", Rel),
    ("rightarrow", "→", Rel),
    ("leftarrow", "←", Rel),
    ("leftrightarrow", "↔", Rel),
    ("Rightarrow", "⇒", Rel),
    ("Leftarrow", "⇐", Rel),
    ("Leftrightarrow", "⇔", Rel),
    ("mapsto", "↦", Rel),
    ("forall", "∀", Plain),
    ("exists", "∃", Plain),
    ("neg", "¬", Plain),
    ("emptyset", "∅", Plain),
    ("varnothing", "∅", Plain),
    ("angle", "∠", Plain),
    ("triangle", "△", Plain),
    ("degree", "°", Plain),
    ("prime", "′", Plain),
    ("ldots", "…", Plain),
    ("cdots", "⋯", Plain),
    ("vdots", "⋮", Plain),
    ("ddots", "⋱", Plain),
    ("therefore", "∴", Plain),
    ("because", "∵", Plain),
    ("checkmark", "✓", Plain),
];

pub fn lookup(name: &str) -> Option<&'static Symbol> {
    SYMBOLS.iter().find(|s| s.name == name)
}

/// 関数名は直立してタイプセットされます。例: `\sin x`。
pub const FUNCTIONS: &[&str] = &[
    "sin", "cos", "tan", "sec", "csc", "cot", "arcsin", "arccos", "arctan", "sinh", "cosh", "tanh",
    "log", "ln", "lg", "exp", "det", "dim", "gcd", "max", "min", "sup", "inf", "arg", "deg", "ker",
    "mod",
];

/// 通常、上下に何かを付けて書かれたシンボル。
pub struct BigOp {
    pub name: &'static str,
    pub glyph: &'static str,
}

pub const BIG_OPS: &[BigOp] = &[
    BigOp {
        name: "sum",
        glyph: "∑",
    },
    BigOp {
        name: "prod",
        glyph: "∏",
    },
    BigOp {
        name: "coprod",
        glyph: "∐",
    },
    BigOp {
        name: "bigcup",
        glyph: "⋃",
    },
    BigOp {
        name: "bigcap",
        glyph: "⋂",
    },
    BigOp {
        name: "lim",
        glyph: "lim",
    },
    BigOp {
        name: "int",
        glyph: "∫",
    },
    BigOp {
        name: "iint",
        glyph: "∬",
    },
    BigOp {
        name: "oint",
        glyph: "∮",
    },
];

pub fn big_op(name: &str) -> Option<&'static BigOp> {
    BIG_OPS.iter().find(|o| o.name == name)
}

pub fn is_function(name: &str) -> bool {
    FUNCTIONS.contains(&name)
}

/// 名前がわかっている場合、`\name` ショートカットが展開されるノード。
pub fn node_for(name: &str) -> Option<Node> {
    match name {
        "stack" | "frac" => Some(ast::stack(Between::Rule)),
        "atop" => Some(ast::stack(Between::Nothing)),
        "arrow" | "xrightarrow" => Some(ast::stack(Between::Arrow('→'))),
        "xleftarrow" => Some(ast::stack(Between::Arrow('←'))),
        "sqrt" => Some(ast::sqrt()),
        "root" | "nthroot" => Some(ast::nth_root()),
        "matrix" => Some(ast::matrix(MatrixKind::Grid, 2, 2)),
        "cases" => Some(ast::matrix(MatrixKind::Cases, 2, 2)),
        _ if big_op(name).is_some() => Some(limits_for(name)),
        _ if is_function(name) => Some(Node::Func(name.to_string())),
        _ if lookup(name).is_some() => Some(Node::Sym(name.to_string())),
        _ => None,
    }
}

/// 名前が表すグリフを使用した、上下にスペースのあるシンボル。
fn limits_for(name: &str) -> Node {
    let glyph = big_op(name).map(|op| op.glyph).unwrap_or(name);
    ast::limits(glyph)
}

/// 直接入力されたグリフが展開される構造。したがって、`√` は `\sqrt` と同様に機能します。構造を必要としないグリフ (`α`、`×`) はプレーン テキストのままです。
pub fn node_for_glyph(glyph: char) -> Option<Node> {
    if glyph == '√' {
        return Some(ast::sqrt());
    }
    let text = glyph.to_string();
    BIG_OPS
        .iter()
        .find(|op| op.glyph == text)
        .map(|op| ast::limits(op.glyph))
}

/// シンボル名をプレーン テキストとして挿入するために印刷されるグリフ。
pub fn glyph_for(name: &str) -> Option<&'static str> {
    lookup(name).map(|symbol| symbol.glyph)
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
