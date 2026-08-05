//! Named symbols the editor understands, and how each one is drawn.

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Variable-like, drawn in italic.
    Ident,
    /// Binary operator, drawn with space on both sides.
    Bin,
    /// Relation, drawn with wider space on both sides.
    Rel,
    /// Punctuation and everything else, drawn upright without extra space.
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

/// Function names typeset upright, e.g. `\sin x`.
pub const FUNCTIONS: &[&str] = &[
    "sin", "cos", "tan", "sec", "csc", "cot", "arcsin", "arccos", "arctan", "sinh", "cosh", "tanh",
    "log", "ln", "lg", "exp", "det", "dim", "gcd", "max", "min", "sup", "inf", "arg", "deg", "ker",
    "mod",
];

/// Large operators. `limits_below` marks the ones whose limits are drawn above
/// and below the glyph instead of beside it.
pub struct BigOp {
    pub name: &'static str,
    pub glyph: &'static str,
    pub limits_below: bool,
}

pub const BIG_OPS: &[BigOp] = &[
    BigOp {
        name: "sum",
        glyph: "∑",
        limits_below: true,
    },
    BigOp {
        name: "prod",
        glyph: "∏",
        limits_below: true,
    },
    BigOp {
        name: "coprod",
        glyph: "∐",
        limits_below: true,
    },
    BigOp {
        name: "bigcup",
        glyph: "⋃",
        limits_below: true,
    },
    BigOp {
        name: "bigcap",
        glyph: "⋂",
        limits_below: true,
    },
    BigOp {
        name: "lim",
        glyph: "lim",
        limits_below: true,
    },
    BigOp {
        name: "int",
        glyph: "∫",
        limits_below: false,
    },
    BigOp {
        name: "iint",
        glyph: "∬",
        limits_below: false,
    },
    BigOp {
        name: "oint",
        glyph: "∮",
        limits_below: false,
    },
];

pub fn big_op(name: &str) -> Option<&'static BigOp> {
    BIG_OPS.iter().find(|o| o.name == name)
}

pub fn is_function(name: &str) -> bool {
    FUNCTIONS.contains(&name)
}
