//! Application shell: toolbar, formula palette, search bar and status bar.

use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{HtmlElement, KeyboardEvent};

use crate::doc;
use crate::ipc;
use crate::math::ast::{self, Delim, MatrixKind, Node};
use crate::math::field;

const UNTITLED: &str = "無題";

#[derive(Clone, Copy)]
struct Shell {
    path: RwSignal<Option<String>>,
    dirty: RwSignal<bool>,
    status: RwSignal<String>,
    stats: RwSignal<(usize, usize)>,
    searching: RwSignal<bool>,
}

impl Shell {
    fn file_name(&self) -> String {
        self.path
            .get()
            .as_deref()
            .and_then(|path| path.rsplit(['/', '\\']).next().map(str::to_string))
            .unwrap_or_else(|| format!("{UNTITLED}.md"))
    }

    fn refresh(&self) {
        self.stats.set(doc::stats());
    }

    fn mark_dirty(&self) {
        if !self.dirty.get_untracked() {
            self.dirty.set(true);
            spawn_local(ipc::set_dirty(true));
        }
        self.refresh();
    }

    fn mark_clean(&self) {
        self.dirty.set(false);
        spawn_local(ipc::set_dirty(false));
        self.refresh();
    }

    fn new_document(&self) {
        if self.dirty.get_untracked() && !confirm("保存されていない変更があります。破棄しますか？")
        {
            return;
        }
        doc::load("");
        self.path.set(None);
        self.status.set(String::new());
        self.mark_clean();
    }

    fn open(&self) {
        if self.dirty.get_untracked() && !confirm("保存されていない変更があります。破棄しますか？")
        {
            return;
        }
        let shell = *self;
        spawn_local(async move {
            let Some(path) = ipc::pick_open_path().await else {
                return;
            };
            match ipc::read_document(&path).await {
                Ok(text) => {
                    doc::load(&text);
                    shell.path.set(Some(path));
                    shell.status.set("開きました".into());
                    shell.mark_clean();
                }
                Err(error) => shell.status.set(error),
            }
        });
    }

    fn save(&self, force_dialog: bool) {
        let shell = *self;
        let current = self.path.get_untracked();
        let default_name = self.file_name();
        spawn_local(async move {
            let path = match current {
                Some(path) if !force_dialog => path,
                _ => match ipc::pick_save_path(&default_name).await {
                    Some(path) => path,
                    None => return,
                },
            };
            let contents = doc::to_markdown();
            match ipc::write_document(&path, &contents).await {
                Ok(()) => {
                    shell.path.set(Some(path));
                    shell.status.set("保存しました".into());
                    shell.mark_clean();
                }
                Err(error) => shell.status.set(error),
            }
        });
    }

    fn export_html(&self) {
        let shell = *self;
        let default_name = self.file_name().replace(".md", "") + ".html";
        spawn_local(async move {
            let Some(path) = ipc::pick_export_path(&default_name).await else {
                return;
            };
            let contents = doc::to_html(&default_name);
            match ipc::write_document(&path, &contents).await {
                Ok(()) => shell.status.set("MathML (HTML) を書き出しました".into()),
                Err(error) => shell.status.set(error),
            }
        });
    }
}

fn confirm(message: &str) -> bool {
    web_sys::window()
        .and_then(|window| window.confirm_with_message(message).ok())
        .unwrap_or(true)
}

/// Inserts a structure into the formula being edited, starting a new formula
/// when the caret is in ordinary text.
fn insert(node: Node) {
    if field::focused_host().is_none() {
        doc::insert_math(false);
    }
    field::insert_into_focused(node);
}

#[component]
pub fn App() -> impl IntoView {
    let shell = Shell {
        path: RwSignal::new(None),
        dirty: RwSignal::new(false),
        status: RwSignal::new(String::new()),
        stats: RwSignal::new((0, 1)),
        searching: RwSignal::new(false),
    };
    let editor_ref = NodeRef::<leptos::html::Div>::new();
    let query = RwSignal::new(String::new());
    let replacement = RwSignal::new(String::new());

    Effect::new(move |_| {
        let Some(element) = editor_ref.get() else {
            return;
        };
        let Ok(element) = element.dyn_into::<HtmlElement>() else {
            return;
        };
        doc::init(&element);
        doc::set_on_change(Box::new(move || shell.mark_dirty()));
        field::set_on_change(Box::new(move || shell.mark_dirty()));
        install_shortcuts(shell);
        element.focus().ok();
        shell.refresh();
        spawn_local(ipc::frontend_ready());
    });

    Effect::new(move |_| {
        let title = format!(
            "{}{} — MathNote",
            if shell.dirty.get() { "*" } else { "" },
            shell.file_name()
        );
        if let Some(document) = web_sys::window().and_then(|w| w.document()) {
            document.set_title(&title);
        }
    });

    view! {
        <div class="app">
            <div class="toolbar">
                <div class="group">
                    <button class="tool" on:mousedown=hold_focus on:click=move |_| shell.new_document()>"新規"</button>
                    <button class="tool" on:mousedown=hold_focus on:click=move |_| shell.open()>"開く"</button>
                    <button class="tool" on:mousedown=hold_focus on:click=move |_| shell.save(false)>"保存"</button>
                    <button class="tool" on:mousedown=hold_focus on:click=move |_| shell.save(true)>"名前を付けて"</button>
                    <button class="tool" on:mousedown=hold_focus on:click=move |_| shell.export_html()>"HTML出力"</button>
                </div>
                <div class="group">
                    <button class="tool" on:mousedown=hold_focus on:click=move |_| doc::insert_math(false) title="数式を挿入 (Ctrl+M)">"数式"</button>
                    <button class="tool" on:mousedown=hold_focus on:click=move |_| doc::insert_math(true) title="独立行の数式 (Ctrl+Shift+M)">"数式(行)"</button>
                    <button class="tool" on:mousedown=hold_focus on:click=move |_| shell.searching.update(|s| *s = !*s) title="検索と置換 (Ctrl+F)">"検索"</button>
                </div>
            </div>

            <Palette/>

            <Show when=move || shell.searching.get()>
                <div class="findbar">
                    <input
                        class="find-input"
                        placeholder="検索"
                        prop:value=move || query.get()
                        on:input=move |ev| query.set(event_target_value(&ev))
                        on:keydown=move |ev: KeyboardEvent| {
                            if ev.key() == "Enter" {
                                doc::find_next(&query.get_untracked());
                            }
                        }
                    />
                    <button class="tool" on:click=move |_| { doc::find_next(&query.get_untracked()); }>"次を検索"</button>
                    <input
                        class="find-input"
                        placeholder="置換後"
                        prop:value=move || replacement.get()
                        on:input=move |ev| replacement.set(event_target_value(&ev))
                    />
                    <button class="tool" on:click=move |_| {
                        let replaced = doc::replace_all(&query.get_untracked(), &replacement.get_untracked());
                        shell.status.set(format!("{replaced} 件置換しました"));
                    }>"すべて置換"</button>
                    <button class="tool" on:click=move |_| shell.searching.set(false)>"閉じる"</button>
                </div>
            </Show>

            <div class="editor" node_ref=editor_ref></div>

            <div class="statusbar">
                <span>{move || shell.file_name()}</span>
                <span>{move || if shell.dirty.get() { "未保存" } else { "保存済み" }}</span>
                <span>{move || {
                    let (characters, lines) = shell.stats.get();
                    format!("{characters} 文字 / {lines} 行")
                }}</span>
                <span class="status-message">{move || shell.status.get()}</span>
            </div>
        </div>
    }
}

/// Keeps the caret inside the formula when a toolbar button is pressed.
fn hold_focus(event: web_sys::MouseEvent) {
    event.prevent_default();
}

#[component]
fn Palette() -> impl IntoView {
    let structures = [
        ("½", "分数 (/)", Structure::Frac),
        ("√", "平方根", Structure::Sqrt),
        ("ⁿ√", "n乗根", Structure::NthRoot),
        ("x²", "上付き (^)", Structure::Sup),
        ("xₙ", "下付き (_)", Structure::Sub),
        ("( )", "括弧", Structure::Paren),
        ("[ ]", "角括弧", Structure::Bracket),
        ("∑", "総和", Structure::Sum),
        ("∏", "総乗", Structure::Prod),
        ("∫", "積分", Structure::Int),
        ("lim", "極限", Structure::Lim),
        ("(⋮)", "行列", Structure::Matrix),
        ("{⋮", "場合分け", Structure::Cases),
    ];
    let symbols = [
        "times",
        "div",
        "cdot",
        "pm",
        "leq",
        "geq",
        "neq",
        "approx",
        "infty",
        "partial",
        "to",
        "Rightarrow",
        "in",
        "subset",
        "cup",
        "cap",
        "forall",
        "exists",
        "angle",
        "degree",
    ];
    let greek = [
        "alpha", "beta", "gamma", "delta", "theta", "lambda", "mu", "pi", "rho", "sigma", "phi",
        "omega", "Gamma", "Delta", "Theta", "Sigma", "Phi", "Omega",
    ];
    let functions = ["sin", "cos", "tan", "log", "ln", "exp"];

    view! {
        <div class="palette">
            <div class="group">
                {structures
                    .into_iter()
                    .map(|(label, tip, structure)| {
                        view! {
                            <button
                                class="pal"
                                title=tip
                                on:mousedown=hold_focus
                                on:click=move |_| insert(structure.node())
                            >
                                {label}
                            </button>
                        }
                    })
                    .collect::<Vec<_>>()}
            </div>
            <div class="group">
                {symbols
                    .into_iter()
                    .map(|name| {
                        let glyph = crate::math::symbols::lookup(name).map(|s| s.glyph).unwrap_or(name);
                        view! {
                            <button
                                class="pal"
                                title=name
                                on:mousedown=hold_focus
                                on:click=move |_| insert(Node::Sym(name.to_string()))
                            >
                                {glyph}
                            </button>
                        }
                    })
                    .collect::<Vec<_>>()}
            </div>
            <div class="group">
                {greek
                    .into_iter()
                    .map(|name| {
                        let glyph = crate::math::symbols::lookup(name).map(|s| s.glyph).unwrap_or(name);
                        view! {
                            <button
                                class="pal"
                                title=name
                                on:mousedown=hold_focus
                                on:click=move |_| insert(Node::Sym(name.to_string()))
                            >
                                {glyph}
                            </button>
                        }
                    })
                    .collect::<Vec<_>>()}
            </div>
            <div class="group">
                {functions
                    .into_iter()
                    .map(|name| {
                        view! {
                            <button
                                class="pal pal-word"
                                on:mousedown=hold_focus
                                on:click=move |_| insert(Node::Func(name.to_string()))
                            >
                                {name}
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
    Frac,
    Sqrt,
    NthRoot,
    Sup,
    Sub,
    Paren,
    Bracket,
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
            Structure::Frac => ast::frac(),
            Structure::Sqrt => ast::sqrt(),
            Structure::NthRoot => ast::nth_root(),
            Structure::Sup => Node::Sup(Vec::new()),
            Structure::Sub => Node::Sub(Vec::new()),
            Structure::Paren => Node::Group {
                delim: Delim::Paren,
                body: Vec::new(),
            },
            Structure::Bracket => Node::Group {
                delim: Delim::Bracket,
                body: Vec::new(),
            },
            Structure::Sum => ast::big_op("sum"),
            Structure::Prod => ast::big_op("prod"),
            Structure::Int => ast::big_op("int"),
            Structure::Lim => ast::big_op("lim"),
            Structure::Matrix => ast::matrix(MatrixKind::Paren, 2, 2),
            Structure::Cases => ast::matrix(MatrixKind::Cases, 2, 2),
        }
    }
}

fn install_shortcuts(shell: Shell) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let handler = Closure::<dyn FnMut(KeyboardEvent)>::new(move |event: KeyboardEvent| {
        if !(event.ctrl_key() || event.meta_key()) {
            if event.key() == "Escape" {
                shell.searching.set(false);
            }
            return;
        }
        let shift = event.shift_key();
        match event.key().to_lowercase().as_str() {
            "n" => {
                event.prevent_default();
                shell.new_document();
            }
            "o" => {
                event.prevent_default();
                shell.open();
            }
            "s" => {
                event.prevent_default();
                shell.save(shift);
            }
            "f" => {
                event.prevent_default();
                shell.searching.set(true);
            }
            "m" => {
                event.prevent_default();
                doc::insert_math(shift);
            }
            _ => {}
        }
    });
    window
        .add_event_listener_with_callback("keydown", handler.as_ref().unchecked_ref())
        .ok();
    handler.forget();
}
