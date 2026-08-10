//! Application shell: toolbar, structure palette, search bar and status bar.

use leptos::prelude::*;
use leptos::reactive::owner::{LocalStorage, Owner};
use leptos::task::spawn_local;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{HtmlElement, KeyboardEvent};

use crate::editor;
use crate::ipc;
use crate::math::ast::{self, Between, MatrixKind, Node};

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

/// One view on the documents: its own tab strip and its own editor. Split view
/// is what makes there be more than one.
#[derive(Clone, Copy)]
struct Pane {
    /// Names this pane in the editor core, once its element exists.
    editor: StoredValue<Option<usize>>,
    tabs: RwSignal<Vec<Tab>>,
    current: RwSignal<usize>,
    /// Keeps the pane's element across renders.
    key: usize,
}

impl Pane {
    fn new(key: usize) -> Pane {
        Pane {
            editor: StoredValue::new(None),
            tabs: RwSignal::new(vec![Tab::new()]),
            current: RwSignal::new(0),
            key,
        }
    }

    fn editor_pane(&self) -> usize {
        self.editor.get_value().unwrap_or_default()
    }

    fn tab(&self) -> Tab {
        let index = self.current.get();
        self.tabs.with(|tabs| tabs[index.min(tabs.len() - 1)])
    }

    fn tab_untracked(&self) -> Tab {
        let index = self.current.get_untracked();
        self.tabs
            .with_untracked(|tabs| tabs[index.min(tabs.len() - 1)])
    }

    /// Takes the shown document off screen, keeping it with its tab.
    fn park(&self) {
        let pane = self.editor_pane();
        self.tab_untracked().parked.set_value(editor::park(pane));
    }
}

#[derive(Clone, Copy)]
struct Shell {
    /// Panes and tabs are made under this owner, which outlives every pane, so
    /// that closing one does not drop the documents it hands over.
    root: StoredValue<Owner, LocalStorage>,
    panes: RwSignal<Vec<Pane>>,
    /// Which pane takes the typing.
    focused: RwSignal<usize>,
    next_key: RwSignal<usize>,
    status: RwSignal<String>,
    stats: RwSignal<(usize, usize)>,
    searching: RwSignal<bool>,
}

impl Shell {
    fn new_tab(&self) -> Tab {
        self.root.with_value(|owner| owner.with(Tab::new))
    }

    fn new_pane(&self, key: usize) -> Pane {
        self.root.with_value(|owner| owner.with(|| Pane::new(key)))
    }

    fn pane(&self) -> Pane {
        let index = self.focused.get();
        self.panes.with(|panes| panes[index.min(panes.len() - 1)])
    }

    fn pane_untracked(&self) -> Pane {
        let index = self.focused.get_untracked();
        self.panes
            .with_untracked(|panes| panes[index.min(panes.len() - 1)])
    }

    fn tab(&self) -> Tab {
        self.pane().tab()
    }

    fn tab_untracked(&self) -> Tab {
        self.pane_untracked().tab_untracked()
    }

    fn file_name(&self) -> String {
        self.tab().name()
    }

    fn refresh(&self) {
        self.stats.set(editor::stats());
    }

    /// Tells the native side whether closing the window would lose work.
    fn sync_dirty(&self) {
        let any = self.panes.with_untracked(|panes| {
            panes.iter().any(|pane| {
                pane.tabs
                    .with_untracked(|tabs| tabs.iter().any(|tab| tab.dirty.get_untracked()))
            })
        });
        spawn_local(ipc::set_dirty(any));
    }

    /// Marks the document of the pane the change came from.
    fn mark_dirty(&self, editor_pane: usize) {
        let pane = self
            .panes
            .with_untracked(|panes| {
                panes
                    .iter()
                    .find(|pane| pane.editor.get_value() == Some(editor_pane))
                    .copied()
            })
            .unwrap_or_else(|| self.pane_untracked());
        let tab = pane.tab_untracked();
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

    /// Sends the typing to the pane a click landed in.
    fn focus_on(&self, pane: Pane) {
        if let Some(index) = self.index_of(pane) {
            self.focused.set(index);
        }
    }

    fn index_of(&self, pane: Pane) -> Option<usize> {
        self.panes
            .with_untracked(|panes| panes.iter().position(|other| other.key == pane.key))
    }

    /// The pane a click or the focus landed in becomes the one in use, so that
    /// the status bar and the shortcuts follow the caret.
    fn note_focus(&self, pane: Pane) {
        let Some(index) = self.index_of(pane) else {
            return;
        };
        if self.focused.get_untracked() == index {
            return;
        }
        self.focused.set(index);
        self.refresh();
    }

    /// Sends the typing to a pane.
    fn focus_pane(&self, index: usize) {
        let Some(pane) = self.panes.with_untracked(|panes| panes.get(index).copied()) else {
            return;
        };
        self.focused.set(index);
        editor::focus_pane(pane.editor_pane());
        self.refresh();
    }

    /// Shows the tab at `index` of `pane`, parking the one on screen.
    fn switch(&self, pane: Pane, index: usize) {
        let current = pane.current.get_untracked();
        let Some(next) = pane.tabs.with_untracked(|tabs| tabs.get(index).copied()) else {
            return;
        };
        if index == current {
            editor::focus_pane(pane.editor_pane());
            return;
        }
        pane.park();
        pane.current.set(index);
        self.show(pane, next);
    }

    /// Puts a tab's document on screen in `pane`, keeping its unsaved mark.
    fn show(&self, pane: Pane, tab: Tab) {
        let dirty = tab.dirty.get_untracked();
        let parked = tab.parked.try_update_value(Option::take).flatten();
        // Drawing the document counts as a change, so the mark is set back.
        editor::restore(pane.editor_pane(), parked);
        tab.dirty.set(dirty);
        self.sync_dirty();
        self.refresh();
        editor::focus_pane(pane.editor_pane());
    }

    /// Opens an empty tab, or reuses the shown one when it is untouched.
    fn add_tab(&self, pane: Pane) -> Tab {
        let shown = pane.tab_untracked();
        if shown.path.get_untracked().is_none() && !shown.dirty.get_untracked() {
            return shown;
        }
        pane.park();
        let tab = self.new_tab();
        pane.tabs.update(|tabs| tabs.push(tab));
        pane.current
            .set(pane.tabs.with_untracked(|tabs| tabs.len() - 1));
        self.show(pane, tab);
        tab
    }

    fn new_document(&self) {
        self.add_tab(self.pane_untracked());
        self.status.set(String::new());
    }

    fn close(&self, pane: Pane, index: usize) {
        let shell = *self;
        spawn_local(async move {
            let Some(tab) = pane.tabs.with_untracked(|tabs| tabs.get(index).copied()) else {
                return;
            };
            if tab.dirty.get_untracked()
                && !ipc::confirm_discard("保存されていない変更があります。破棄しますか？").await
            {
                return;
            }
            let current = pane.current.get_untracked();
            if pane.tabs.with_untracked(Vec::len) == 1 {
                // The last tab stays, emptied, so there is always a document.
                tab.path.set(None);
                editor::restore(pane.editor_pane(), None);
                tab.dirty.set(false);
                shell.sync_dirty();
                shell.refresh();
                editor::focus_pane(pane.editor_pane());
                return;
            }
            pane.tabs.update(|tabs| {
                tabs.remove(index);
            });
            let last = pane.tabs.with_untracked(|tabs| tabs.len() - 1);
            if index == current {
                let next = index.min(last);
                pane.current.set(next);
                shell.show(pane, pane.tabs.with_untracked(|tabs| tabs[next]));
            } else {
                pane.current.set(if index < current {
                    current - 1
                } else {
                    current.min(last)
                });
                shell.sync_dirty();
            }
        });
    }

    /// Adds a pane beside the shown one, or removes the shown one.
    fn toggle_split(&self) {
        if self.panes.with_untracked(Vec::len) > 1 {
            self.unsplit();
            return;
        }
        let key = self.next_key.get_untracked();
        self.next_key.set(key + 1);
        let pane = self.new_pane(key);
        self.panes.update(|panes| panes.push(pane));
        self.focus_pane(self.panes.with_untracked(|panes| panes.len() - 1));
    }

    /// Keeps the pane in use and drops the other one. Its tabs move over, so no
    /// document is closed.
    fn unsplit(&self) {
        let count = self.panes.with_untracked(Vec::len);
        if count < 2 {
            return;
        }
        let staying = self.focused.get_untracked().min(count - 1);
        let index = if staying == 0 { 1 } else { 0 };
        let (going, staying) = self
            .panes
            .with_untracked(|panes| (panes[index], panes[staying]));
        going.park();
        // The tabs move first: the pane they came from owns them until it goes.
        let moved = going.tabs.get_untracked();
        staying.tabs.update(|tabs| tabs.extend(moved));
        self.panes.update(|panes| {
            panes.remove(index);
        });
        editor::close_pane(going.editor_pane());
        self.focused.set(0);
        editor::focus_pane(staying.editor_pane());
        self.sync_dirty();
        self.refresh();
    }

    fn open(&self) {
        let shell = *self;
        spawn_local(async move {
            let Some(path) = ipc::pick_open_path().await else {
                return;
            };
            match ipc::read_document(&path).await {
                Ok(text) => {
                    let pane = shell.pane_untracked();
                    let tab = shell.add_tab(pane);
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
        root: StoredValue::new_local(Owner::current().expect("the app has an owner")),
        panes: RwSignal::new(vec![Pane::new(0)]),
        focused: RwSignal::new(0),
        next_key: RwSignal::new(1),
        status: RwSignal::new(String::new()),
        stats: RwSignal::new((0, 1)),
        searching: RwSignal::new(false),
    };
    let query = RwSignal::new(String::new());
    let replacement = RwSignal::new(String::new());
    let regex = RwSignal::new(false);
    let case_sensitive = RwSignal::new(false);
    let options = move || editor::SearchOptions {
        regex: regex.get_untracked(),
        case_sensitive: case_sensitive.get_untracked(),
    };

    Effect::new(move |_| {
        editor::set_on_change(Box::new(move |pane| shell.mark_dirty(pane)));
        install_shortcuts(shell);
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
                    <button
                        class="tool"
                        on:mousedown=hold_focus
                        on:click=move |_| shell.toggle_split()
                        title="左右に分割 / 解除 (Ctrl+\\)"
                    >
                        {move || if shell.panes.get().len() > 1 { "分割解除" } else { "分割" }}
                    </button>
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

            <div class="panes">
                <For each=move || shell.panes.get() key=|pane| pane.key let:pane>
                    <PaneView shell=shell pane=pane/>
                </For>
            </div>

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

/// A tab strip and the editor below it.
#[component]
fn PaneView(shell: Shell, pane: Pane) -> impl IntoView {
    let editor_ref = NodeRef::<leptos::html::Div>::new();

    Effect::new(move |_| {
        let Some(element) = editor_ref.get() else {
            return;
        };
        let Ok(element) = element.dyn_into::<HtmlElement>() else {
            return;
        };
        if pane.editor.get_value().is_some() {
            return;
        }
        pane.editor.set_value(editor::init(&element));
        // A pane made by splitting takes the typing right away.
        if let Some(index) = shell.index_of(pane) {
            if index == shell.focused.get_untracked() {
                shell.focus_pane(index);
            }
        }
    });

    let focused = move || {
        shell
            .panes
            .with(|panes| panes.get(shell.focused.get()).map(|other| other.key))
            == Some(pane.key)
    };

    view! {
        <div
            class=move || if focused() { "pane pane-focused" } else { "pane" }
            on:mousedown=move |_| shell.note_focus(pane)
            on:focusin=move |_| shell.note_focus(pane)
        >
            <Tabs shell=shell pane=pane/>
            <div class="editor" node_ref=editor_ref></div>
        </div>
    }
}

/// One button per open file, with the unsaved mark and a way to close it.
#[component]
fn Tabs(shell: Shell, pane: Pane) -> impl IntoView {
    view! {
        <div class="tabbar">
            {move || {
                let current = pane.current.get();
                pane.tabs
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
                                    on:click=move |_| {
                                        shell.focus_on(pane);
                                        shell.switch(pane, index);
                                    }
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
                                    on:click=move |_| {
                                        shell.focus_on(pane);
                                        shell.close(pane, index);
                                    }
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
                on:click=move |_| {
                    shell.focus_on(pane);
                    shell.new_document();
                }
            >
                "+"
            </button>
        </div>
    }
}

#[component]
fn Palette() -> impl IntoView {
    // Only what plain text cannot hold: everything else is typed as usual.
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
                let pane = shell.pane_untracked();
                shell.close(pane, pane.current.get_untracked());
            }
            "\\" => {
                event.prevent_default();
                shell.toggle_split();
            }
            "tab" => {
                event.prevent_default();
                let pane = shell.pane_untracked();
                let count = pane.tabs.with_untracked(Vec::len);
                let current = pane.current.get_untracked();
                let next = if shift {
                    (current + count - 1) % count
                } else {
                    (current + 1) % count
                };
                shell.switch(pane, next);
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
