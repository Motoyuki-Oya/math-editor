//! 構造パレット: プレーン テキストでは保持できないもののみ。それ以外はすべて通常どおりに入力されます。

use leptos::prelude::*;

use super::hold_focus;
use super::shell::Pane;
use crate::editor;
use crate::structure::ast::{self, Between, MatrixKind, Node};

#[component]
pub(super) fn Palette(pane: Pane) -> impl IntoView {
    // プレーン テキストで保持できないもののみです。他のすべては通常どおりに入力されます。
    let structures = [
        ("½", "分数", Structure::Stack),
        ("ⁿ", "線のない上下", Structure::Bare),
        ("→", "矢印の上下", Structure::Arrow),
        ("√", "ルート", Structure::Sqrt),
        ("ⁿ√", "n 乗根", Structure::NthRoot),
        ("x²", "上付き", Structure::Sup),
        ("xₙ", "下付き", Structure::Sub),
        ("∑", "和（上下に範囲）", Structure::Sum),
        ("∏", "積（上下に範囲）", Structure::Prod),
        ("∫", "積分（上下に範囲）", Structure::Int),
        ("lim", "極限（下に近づく先）", Structure::Lim),
        ("[⋮]", "行列（行の追加は Alt+Enter）", Structure::Matrix),
        ("{⋮", "場合分け（行の追加は Alt+Enter）", Structure::Cases),
    ];

    let close = move |_| pane.palette.set(false);

    view! {
        <div class="palette" on:mousedown=move |ev| ev.stop_propagation()>
            <div class="palette-header">
                <span class="palette-title">"構造パレット"</span>
                <button class="find-icon-btn find-close-btn" title="閉じる (Ctrl+M)" on:click=close>
                    "✕"
                </button>
            </div>
            <div class="palette-grid">
                <button class="pal" title="上に注釈" on:mousedown=hold_focus on:click=move |_| editor::annotate(true)>"x̅"</button>
                <button class="pal" title="下に注釈" on:mousedown=hold_focus on:click=move |_| editor::annotate(false)>"x̲"</button>
                {structures
                    .into_iter()
                    .map(|(label, tip, structure)| {
                        view! {
                            <button
                                class="pal"
                                title=tip
                                on:mousedown=hold_focus
                                on:click=move |_| editor::insert_node(structure.node())
                            >
                                {label}
                            </button>
                        }
                    })
                    .collect::<Vec<_>>()}
            </div>
        </div>
    }
}

#[derive(Clone, Copy)]
enum Structure {
    Stack,
    Bare,
    Arrow,
    Sqrt,
    NthRoot,
    Sup,
    Sub,
    Sum,
    Prod,
    Int,
    Lim,
    Matrix,
    Cases,
}

impl Structure {
    fn node(self) -> Node {
        match self {
            Structure::Stack => ast::stack(Between::Rule),
            Structure::Bare => ast::stack(Between::Nothing),
            Structure::Arrow => ast::stack(Between::Arrow('→')),
            Structure::Sqrt => ast::sqrt(),
            Structure::NthRoot => ast::nth_root(),
            Structure::Sup => Node::sup(Vec::new()),
            Structure::Sub => Node::sub(Vec::new()),
            Structure::Sum => ast::limits("∑"),
            Structure::Prod => ast::limits("∏"),
            Structure::Int => ast::limits("∫"),
            Structure::Lim => ast::limits("lim"),
            Structure::Matrix => ast::matrix(MatrixKind::Grid, 2, 2),
            Structure::Cases => ast::matrix(MatrixKind::Cases, 2, 1),
        }
    }
}
