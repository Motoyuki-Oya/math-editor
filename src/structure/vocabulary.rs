//! 語彙: 編集者が知っているすべての名前とグリフ、およびそれぞれが何を表すか。ショートカットは同じ語彙からの名前にすぎないため、シンボル テーブルと `\name` ショートカットは一緒に存在します。同じテーブルがテキスト エディタとアイランド エディタに機能するため、`\sqrt` はどこに入力しても同じように動作します。

use super::ast::{self, Between, MatrixKind, Node};

pub struct Symbol {
    pub name: &'static str,
    pub glyph: &'static str,
}

macro_rules! symbols {
    ($(($name:literal, $glyph:literal)),* $(,)?) => {
        pub const SYMBOLS: &[Symbol] = &[
            $(Symbol { name: $name, glyph: $glyph }),*
        ];
    };
}

symbols![
    ("alpha", "α"),
    ("beta", "β"),
    ("gamma", "γ"),
    ("delta", "δ"),
    ("epsilon", "ε"),
    ("varepsilon", "ε"),
    ("zeta", "ζ"),
    ("eta", "η"),
    ("theta", "θ"),
    ("vartheta", "ϑ"),
    ("iota", "ι"),
    ("kappa", "κ"),
    ("lambda", "λ"),
    ("mu", "μ"),
    ("nu", "ν"),
    ("xi", "ξ"),
    ("pi", "π"),
    ("rho", "ρ"),
    ("sigma", "σ"),
    ("tau", "τ"),
    ("upsilon", "υ"),
    ("phi", "φ"),
    ("varphi", "ϕ"),
    ("chi", "χ"),
    ("psi", "ψ"),
    ("omega", "ω"),
    ("Gamma", "Γ"),
    ("Delta", "Δ"),
    ("Theta", "Θ"),
    ("Lambda", "Λ"),
    ("Xi", "Ξ"),
    ("Pi", "Π"),
    ("Sigma", "Σ"),
    ("Phi", "Φ"),
    ("Psi", "Ψ"),
    ("Omega", "Ω"),
    ("infty", "∞"),
    ("partial", "∂"),
    ("nabla", "∇"),
    ("hbar", "ℏ"),
    ("ell", "ℓ"),
    ("Re", "ℜ"),
    ("Im", "ℑ"),
    ("aleph", "ℵ"),
    ("times", "×"),
    ("div", "÷"),
    ("cdot", "⋅"),
    ("pm", "±"),
    ("mp", "∓"),
    ("ast", "∗"),
    ("star", "⋆"),
    ("circ", "∘"),
    ("oplus", "⊕"),
    ("otimes", "⊗"),
    ("cup", "∪"),
    ("cap", "∩"),
    ("setminus", "∖"),
    ("leq", "≤"),
    ("le", "≤"),
    ("geq", "≥"),
    ("ge", "≥"),
    ("neq", "≠"),
    ("ne", "≠"),
    ("approx", "≈"),
    ("sim", "∼"),
    ("simeq", "≃"),
    ("equiv", "≡"),
    ("propto", "∝"),
    ("ll", "≪"),
    ("gg", "≫"),
    ("subset", "⊂"),
    ("subseteq", "⊆"),
    ("supset", "⊃"),
    ("supseteq", "⊇"),
    ("in", "∈"),
    ("notin", "∉"),
    ("ni", "∋"),
    ("perp", "⊥"),
    ("parallel", "∥"),
    ("mid", "∣"),
    ("to", "→"),
    ("rightarrow", "→"),
    ("leftarrow", "←"),
    ("leftrightarrow", "↔"),
    ("Rightarrow", "⇒"),
    ("Leftarrow", "⇐"),
    ("Leftrightarrow", "⇔"),
    ("mapsto", "↦"),
    ("forall", "∀"),
    ("exists", "∃"),
    ("neg", "¬"),
    ("emptyset", "∅"),
    ("varnothing", "∅"),
    ("angle", "∠"),
    ("triangle", "△"),
    ("degree", "°"),
    ("prime", "′"),
    ("ldots", "…"),
    ("cdots", "⋯"),
    ("vdots", "⋮"),
    ("ddots", "⋱"),
    ("therefore", "∴"),
    ("because", "∵"),
    ("checkmark", "✓"),
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

/// `\name`が二次元構造を表す場合、そのNodeを返します。
pub fn structure_for(name: &str) -> Option<Node> {
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
        _ => None,
    }
}

/// `\name`が通常文字を表す場合、置き換える文字列を返します。
pub fn text_for(name: &str) -> Option<String> {
    if is_function(name) {
        Some(name.to_string())
    } else {
        glyph_for(name).map(str::to_string)
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
    let text = if glyph == 'Σ' { '∑' } else { glyph }.to_string();
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
    use crate::structure::ast::NodeKind;

    #[test]
    fn structures_symbols_and_functions_are_known() {
        assert!(matches!(
            structure_for("sqrt").map(|n| n.kind),
            Some(NodeKind::Sqrt { .. })
        ));
        assert!(matches!(
            structure_for("sum").map(|n| n.kind),
            Some(NodeKind::BigOp(_))
        ));
        assert!(matches!(
            structure_for("cases").map(|n| n.kind),
            Some(NodeKind::Matrix { .. })
        ));
        assert_eq!(text_for("alpha"), Some("α".into()));
        assert_eq!(text_for("sin"), Some("sin".into()));
    }

    #[test]
    fn unknown_names_are_rejected() {
        assert!(structure_for("notacommand").is_none());
        assert!(text_for("notacommand").is_none());
    }

    #[test]
    fn only_structural_glyphs_expand() {
        assert!(matches!(
            node_for_glyph('√').map(|n| n.kind),
            Some(NodeKind::Sqrt { .. })
        ));
        assert!(matches!(
            node_for_glyph('∑').map(|n| n.kind),
            Some(NodeKind::BigOp(_))
        ));
        assert!(matches!(
            node_for_glyph('Σ').map(|n| n.kind),
            Some(NodeKind::BigOp(_))
        ));
        assert!(node_for_glyph('α').is_none());
        assert!(node_for_glyph('a').is_none());
    }

    #[test]
    fn symbol_names_have_glyphs() {
        assert_eq!(glyph_for("alpha"), Some("α"));
        assert_eq!(glyph_for("nope"), None);
    }
}
