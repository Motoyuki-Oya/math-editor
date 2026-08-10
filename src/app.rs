//! Application shell: toolbar, structure palette, search bar and status bar.

use leptos::prelude::*;
use leptos::reactive::owner::LocalStorage;
use leptos::task::spawn_local;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{HtmlElement, KeyboardEvent};

use crate::editor;
use crate::ipc;
use crate::math::ast::{self, Between, MatrixKind, Node};
use crate::math::commands;

const UNTITLED: &str = "無題";

/// One open file. The document itself lives in the editor while the tab is
/// shown, and is parked here while another tab is.
#[derive(Clone, Copy)]
struct Tab {
    path: RwSignal<Option<String>>,
    dirty: RwSignal<bool>,
    parked: StoredValue<Option<editor::Parked>, LocalStorage>,
}

impl Tab {
    fn new() -> Tab {
        Tab {
            path: RwSignal::new(None),
            dirty: RwSignal::new(false),
            parked: StoredValue::new_local(None),
        }
    }

    fn name(&self) -> String {
        self.path
            .get()
            .as_deref()
            .and_then(|path| path.rsplit(['/', '\\']).next().map(str::to_string))
            .unwrap_or_else(|| format!("{UNTITLED}.txt"))
    }
}

#[derive(Clone, Copy)]
struct Shell {
    tabs: RwSignal<Vec<Tab>>,
    current: RwSignal<usize>,
    status: RwSignal<String>,
    stats: RwSignal<(usize, usize)>,
    searching: RwSignal<bool>,
}

impl Shell {
    fn tab(&self) -> Tab {
        let index = self.current.get();
        self.tabs.with(|tabs| tabs[index.min(tabs.len() - 1)])
    }

    fn tab_untracked(&self) -> Tab {
        let index = self.current.get_untracked();
        self.tabs
            .with_untracked(|tabs| tabs[index.min(tabs.len() - 1)])
    }

    fn file_name(&self) -> String {
        self.tab().name()
    }

    fn refresh(&self) {
        self.stats.set(editor::stats());
    }

    /// Tells the native side whether closing the window would lose work.
    fn sync_dirty(&self) {
        let any = self
            .tabs
            .with_untracked(|tabs| tabs.iter().any(|tab| tab.dirty.get_untracked()));
        spawn_local(ipc::set_dirty(any));
    }

    fn mark_dirty(&self) {
        let tab = self.tab_untracked();
        if !tab.dirty.get_untracked() {
            tab.dirty.set(true);
            self.sync_dirty();
        }
        self.refresh();
    }

    fn mark_clean(&self) {
        self.tab_untracked().dirty.set(false);
        self.sync_dirty();
        self.refresh();
    }

    /// Shows the tab at `index`, parking the one on screen.
    fn switch(&self, index: usize) {
        let current = self.current.get_untracked();
        let Some(next) = self.tabs.with_untracked(|tabs| tabs.get(index).copied()) else {
            return;
        };
        if index == current {
            editor::focus();
            return;
        }
        self.tab_untracked().parked.set_value(editor::park());
        self.current.set(index);
        self.show(next);
    }

    /// Puts a tab's document back on screen, keeping its unsaved mark.
    fn show(&self, tab: Tab) {
        let dirty = tab.dirty.get_untracked();
        let parked = tab.parked.try_update_value(Option::take).flatten();
        // Drawing the document counts as a change, so the mark is set back.
        editor::restore(parked);
        tab.dirty.set(dirty);
        self.sync_dirty();
        self.refresh();
        editor::focus();
    }

    /// Opens an empty tab, or reuses the shown one when it is untouched.
    fn add_tab(&self) -> Tab {
        let shown = self.tab_untracked();
        if shown.path.get_untracked().is_none() && !shown.dirty.get_untracked() {
            return shown;
        }
        shown.parked.set_value(editor::park());
        let tab = Tab::new();
        self.tabs.update(|tabs| tabs.push(tab));
        self.current
            .set(self.tabs.with_untracked(|tabs| tabs.len() - 1));
        self.show(tab);
        tab
    }

    fn new_document(&self) {
        self.add_tab();
        self.status.set(String::new());
    }

    fn close(&self, index: usize) {
        let shell = *self;
        spawn_local(async move {
            let Some(tab) = shell.tabs.with_untracked(|tabs| tabs.get(index).copied()) else {
                return;
            };
            if tab.dirty.get_untracked()
                && !ipc::confirm_discard("保存されていない変更があります。破棄しますか？").await
            {
                return;
            }
            let current = shell.current.get_untracked();
            if shell.tabs.with_untracked(Vec::len) == 1 {
                // The last tab stays, emptied, so there is always a document.
                tab.path.set(None);
                editor::restore(None);
                tab.dirty.set(false);
                shell.sync_dirty();
                shell.refresh();
                editor::focus();
                return;
            }
            shell.tabs.update(|tabs| {
                tabs.remove(index);
            });
            let last = shell.tabs.with_untracked(|tabs| tabs.len() - 1);
            if index == current {
                let next = index.min(last);
                shell.current.set(next);
                shell.show(shell.tabs.with_untracked(|tabs| tabs[next]));
            } else {
                shell.current.set(if index < current {
                    current - 1
                } else {
                    current.min(last)
                });
                shell.sync_dirty();
            }
        });
    }

    fn open(&self) {
        let shell = *self;
        spawn_local(async move {
            let Some(path) = ipc::pick_open_path().await else {
                return;
            };
            match ipc::read_document(&path).await {
                Ok(text) => {
                    let tab = shell.add_tab();
                    editor::load(&text);
                    tab.path.set(Some(path));
                    shell.status.set("開きました".into());
                    shell.mark_clean();
                }
                Err(error) => shell.status.set(error),
            }
        });
    }

    fn save(&self, force_dialog: bool) {
        let shell = *self;
        let tab = self.tab_untracked();
        let current = tab.path.get_untracked();
        let default_name = tab.name();
        spawn_local(async move {
            let path = match current {
                Some(path) if !force_dialog => path,
                _ => match ipc::pick_save_path(&default_name).await {
                    Some(path) => path,
                    None => return,
                },
            };
            let contents = editor::to_document();
            match ipc::write_document(&path, &contents).await {
                Ok(()) => {
                    tab.path.set(Some(path));
                    shell.status.set("保存しました".into());
                    shell.mark_clean();
                }
                Err(error) => shell.status.set(error),
            }
        });
    }
}

#[component]
pub fn App() -> impl IntoView {
    let shell = Shell {
        tabs: RwSignal::new(vec![Tab::new()]),
        current: RwSignal::new(0),
        status: RwSignal::new(String::new()),
        stats: RwSignal::new((0, 1)),
        searching: RwSignal::new(false),
    };
    let editor_ref = NodeRef::<leptos::html::Div>::new();
    let query = RwSignal::new(String::new());
    let replacement = RwSignal::new(String::new());
    let regex = RwSignal::new(false);
    let case_sensitive = RwSignal::new(false);
    let options = move || editor::SearchOptions {
        regex: regex.get_untracked(),
        case_sensitive: case_sensitive.get_untracked(),
    };

    Effect::new(move |_| {
        let Some(element) = editor_ref.get() else {
            return;
        };
        let Ok(element) = element.dyn_into::<HtmlElement>() else {
            return;
        };
        editor::init(&element);
        editor::set_on_change(Box::new(move || shell.mark_dirty()));
        install_shortcuts(shell);
        editor::focus();
        shell.refresh();
        spawn_local(ipc::frontend_ready());
    });

    Effect::new(move |_| {
        let title = format!(
            "{}{} — MathNote",
            if shell.tab().dirty.get() { "*" } else { "" },
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
                    <button class="tool" on:mousedown=hold_focus on:click=move |_| shell.new_document() title="新しいタブ (Ctrl+T)">"新規"</button>
                    <button class="tool" on:mousedown=hold_focus on:click=move |_| shell.open()>"開く"</button>
                    <button class="tool" on:mousedown=hold_focus on:click=move |_| shell.save(false)>"保存"</button>
                    <button class="tool" on:mousedown=hold_focus on:click=move |_| shell.save(true)>"名前を付けて"</button>
                </div>
                <div class="group">
                    <button class="tool" on:mousedown=hold_focus on:click=move |_| editor::insert_math() title="構造を挿入 (Ctrl+M)">"構造"</button>
                    <button class="tool" on:mousedown=hold_focus on:click=move |_| shell.searching.update(|s| *s = !*s) title="検索と置換 (Ctrl+F)">"検索"</button>
                </div>
            </div>

            <Tabs shell=shell/>

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
                                editor::find_next(&query.get_untracked(), options());
                            }
                        }
                    />
                    <button class="tool" on:click=move |_| { editor::find_next(&query.get_untracked(), options()); }>"次を検索"</button>
                    <input
                        class="find-input"
                        placeholder="置換後"
                        prop:value=move || replacement.get()
                        on:input=move |ev| replacement.set(event_target_value(&ev))
                    />
                    <button class="tool" on:click=move |_| {
                        let replaced = editor::replace_all(&query.get_untracked(), &replacement.get_untracked(), options());
                        shell.status.set(format!("{replaced} 件置換しました"));
                    }>"すべて置換"</button>
                    <label class="find-toggle" title="大文字小文字を区別">
                        <input
                            type="checkbox"
                            prop:checked=move || case_sensitive.get()
                            on:change=move |ev| case_sensitive.set(event_target_checked(&ev))
                        />
                        "Aa"
                    </label>
                    <label class="find-toggle" title="正規表現（置換では $1 で後方参照）">
                        <input
                            type="checkbox"
                            prop:checked=move || regex.get()
                            on:change=move |ev| regex.set(event_target_checked(&ev))
                        />
                        ".*"
                    </label>
                    <button class="tool" on:click=move |_| shell.searching.set(false)>"閉じる"</button>
                </div>
            </Show>

            <div class="editor" node_ref=editor_ref></div>

            <div class="statusbar">
                <span>{move || shell.file_name()}</span>
                <span>{move || if shell.tab().dirty.get() { "未保存" } else { "保存済み" }}</span>
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

/// One button per open file, with the unsaved mark and a way to close it.
#[component]
fn Tabs(shell: Shell) -> impl IntoView {
    view! {
        <div class="tabbar">
            {move || {
                let current = shell.current.get();
                shell
                    .tabs
                    .get()
                    .into_iter()
                    .enumerate()
                    .map(|(index, tab)| {
                        view! {
                            <span class=move || {
                                if index == current { "tab tab-current" } else { "tab" }
                            }>
                                <button
                                    class="tab-name"
                                    on:mousedown=hold_focus
                                    on:click=move |_| shell.switch(index)
                                >
                                    {move || {
                                        format!(
                                            "{}{}",
                                            if tab.dirty.get() { "*" } else { "" },
                                            tab.name(),
                                        )
                                    }}
                                </button>
                                <button
                                    class="tab-close"
                                    title="閉じる (Ctrl+W)"
                                    on:mousedown=hold_focus
                                    on:click=move |_| shell.close(index)
                                >
                                    "×"
                                </button>
                            </span>
                        }
                    })
                    .collect::<Vec<_>>()
            }}
            <button
                class="tab-add"
                title="新しいタブ (Ctrl+T)"
                on:mousedown=hold_focus
                on:click=move |_| shell.new_document()
            >
                "+"
            </button>
        </div>
    }
}

#[component]
fn Palette() -> impl IntoView {
    let structures = [
        ("½", "罫線とその上下  $(上/下)", Structure::Stack),
        ("ⁿ", "罫線なしの上下  $(上 - 下)", Structure::Bare),
        ("→", "矢印とその上下  $(上 → 下)", Structure::Arrow),
        ("√", "ルート  $(√ x)", Structure::Sqrt),
        ("ⁿ√", "n乗根  $(√[n] x)", Structure::NthRoot),
        ("x²", "上付き  x$(^ 3)", Structure::Sup),
        ("xₙ", "下付き  x$(_ i)", Structure::Sub),
        ("∑", "記号の上下  $(↨ Σ, 上, 下)", Structure::Sum),
        ("∏", "記号の上下  $(↨ ∏, 上, 下)", Structure::Prod),
        ("∫", "記号の上下  $(↨ ∫, 上, 下)", Structure::Int),
        ("lim", "記号の上下  $(↨ lim, 上, 下)", Structure::Lim),
        (
            "[⋮]",
            "格子状の並び  $([a, b][c, d])  行追加は Alt+Enter",
            Structure::Matrix,
        ),
        (
            "{⋮",
            "場合分け  $({[…][…])  行追加は Alt+Enter",
            Structure::Cases,
        ),
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
                                on:click=move |_| editor::insert_node(structure.node())
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
                    .chain(greek)
                    .map(|name| {
                        let glyph = commands::glyph_for(name).unwrap_or(name);
                        view! {
                            <button
                                class="pal"
                                title=name
                                on:mousedown=hold_focus
                                on:click=move |_| editor::insert_plain(glyph)
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
                                on:click=move |_| editor::insert_plain(name)
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
            Structure::Sup => Node::Sup(Vec::new()),
            Structure::Sub => Node::Sub(Vec::new()),
            Structure::Sum => ast::limits("∑"),
            Structure::Prod => ast::limits("∏"),
            Structure::Int => ast::limits("∫"),
            Structure::Lim => ast::limits("lim"),
            Structure::Matrix => ast::matrix(MatrixKind::Grid, 2, 2),
            Structure::Cases => ast::matrix(MatrixKind::Cases, 2, 2),
        }
    }
}

fn install_shortcuts(shell: Shell) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let handler = Closure::<dyn FnMut(KeyboardEvent)>::new(move |event: KeyboardEvent| {
        // The formula being edited keeps its own history, and handles the key
        // itself before it reaches here.
        if event.default_prevented() {
            return;
        }
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
            "t" => {
                event.prevent_default();
                shell.new_document();
            }
            "w" => {
                event.prevent_default();
                shell.close(shell.current.get_untracked());
            }
            "tab" => {
                event.prevent_default();
                let count = shell.tabs.with_untracked(Vec::len);
                let current = shell.current.get_untracked();
                let next = if shift {
                    (current + count - 1) % count
                } else {
                    (current + 1) % count
                };
                shell.switch(next);
            }
            "m" => {
                event.prevent_default();
                editor::insert_math();
            }
            "z" => {
                event.prevent_default();
                if shift {
                    editor::redo();
                } else {
                    editor::undo();
                }
            }
            "y" => {
                event.prevent_default();
                editor::redo();
            }
            _ => {}
        }
    });
    window
        .add_event_listener_with_callback("keydown", handler.as_ref().unchecked_ref())
        .ok();
    handler.forget();
}
