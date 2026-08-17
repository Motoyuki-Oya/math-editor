//! The state of the application: the panes, the tabs, the files they show,
//! and what opening, saving and closing them does.

use leptos::prelude::*;
use leptos::reactive::owner::{LocalStorage, Owner};
use leptos::task::spawn_local;

use super::drafts;
use crate::editor;
use crate::ipc;

const UNTITLED: &str = "無題";

thread_local! {
    /// Names the next tab. A tab's number is also the name of its draft, so it
    /// has to outlive the tab itself: a restored draft keeps its number.
    static NEXT_ID: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn next_id() -> usize {
    let id = NEXT_ID.get();
    NEXT_ID.set(id + 1);
    id
}

/// One open file. The document itself lives in the editor while the tab is
/// shown, and is parked here while another tab is.
#[derive(Clone, Copy)]
pub(super) struct Tab {
    /// Names this tab's draft on disk.
    pub(super) id: RwSignal<usize>,
    pub(super) path: RwSignal<Option<String>>,
    pub(super) dirty: RwSignal<bool>,
    pub(super) parked: StoredValue<Option<editor::Parked>, LocalStorage>,
}

impl Tab {
    pub(super) fn new() -> Tab {
        Tab {
            id: RwSignal::new(next_id()),
            path: RwSignal::new(None),
            dirty: RwSignal::new(false),
            parked: StoredValue::new_local(None),
        }
    }

    pub(super) fn name(&self) -> String {
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
pub(super) struct Pane {
    /// Names this pane in the editor core, once its element exists.
    pub(super) editor: StoredValue<Option<usize>>,
    pub(super) tabs: RwSignal<Vec<Tab>>,
    pub(super) current: RwSignal<usize>,
    /// Keeps the pane's element across renders.
    pub(super) key: usize,
}

impl Pane {
    pub(super) fn new(key: usize) -> Pane {
        Pane {
            editor: StoredValue::new(None),
            tabs: RwSignal::new(vec![Tab::new()]),
            current: RwSignal::new(0),
            key,
        }
    }

    pub(super) fn editor_pane(&self) -> usize {
        self.editor.get_value().unwrap_or_default()
    }

    pub(super) fn tab(&self) -> Tab {
        let index = self.current.get();
        self.tabs.with(|tabs| tabs[index.min(tabs.len() - 1)])
    }

    pub(super) fn tab_untracked(&self) -> Tab {
        let index = self.current.get_untracked();
        self.tabs
            .with_untracked(|tabs| tabs[index.min(tabs.len() - 1)])
    }

    /// Takes the shown document off screen, keeping it with its tab.
    pub(super) fn park(&self) {
        let pane = self.editor_pane();
        let tab = self.tab_untracked();
        // Written now: once the document is off screen the draft cannot read
        // it from the pane any more.
        if tab.dirty.get_untracked() {
            drafts::write(tab, pane);
        }
        tab.parked.set_value(editor::park(pane));
    }
}

#[derive(Clone, Copy)]
pub(super) struct Shell {
    /// Panes and tabs are made under this owner, which outlives every pane, so
    /// that closing one does not drop the documents it hands over.
    pub(super) root: StoredValue<Owner, LocalStorage>,
    pub(super) panes: RwSignal<Vec<Pane>>,
    /// Which pane takes the typing.
    pub(super) focused: RwSignal<usize>,
    pub(super) next_key: RwSignal<usize>,
    pub(super) status: RwSignal<String>,
    pub(super) stats: RwSignal<(usize, usize)>,
    pub(super) searching: RwSignal<bool>,
    /// Whether the settings are on screen.
    pub(super) preferences: RwSignal<bool>,
    /// The field of the find bar waiting for the cursor, once it is on screen.
    pub(super) find_focus: RwSignal<Option<Field>>,
}

/// A field of the find bar, named so a shortcut can ask for it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Field {
    Query,
    Replacement,
}

impl Shell {
    /// Opens the find bar with the cursor in `field`, which is what Ctrl+F and
    /// Ctrl+R do.
    pub(super) fn find(&self, field: Field) {
        self.searching.set(true);
        self.find_focus.set(Some(field));
    }

    pub(super) fn new_tab(&self) -> Tab {
        self.root.with_value(|owner| owner.with(Tab::new))
    }

    pub(super) fn new_pane(&self, key: usize) -> Pane {
        self.root.with_value(|owner| owner.with(|| Pane::new(key)))
    }

    pub(super) fn pane(&self) -> Pane {
        let index = self.focused.get();
        self.panes.with(|panes| panes[index.min(panes.len() - 1)])
    }

    pub(super) fn pane_untracked(&self) -> Pane {
        let index = self.focused.get_untracked();
        self.panes
            .with_untracked(|panes| panes[index.min(panes.len() - 1)])
    }

    pub(super) fn tab(&self) -> Tab {
        self.pane().tab()
    }

    pub(super) fn tab_untracked(&self) -> Tab {
        self.pane_untracked().tab_untracked()
    }

    pub(super) fn file_name(&self) -> String {
        self.tab().name()
    }

    pub(super) fn refresh(&self) {
        self.stats.set(editor::stats());
    }

    /// Tells the native side whether closing the window would lose work.
    pub(super) fn sync_dirty(&self) {
        let any = self.panes.with_untracked(|panes| {
            panes.iter().any(|pane| {
                pane.tabs
                    .with_untracked(|tabs| tabs.iter().any(|tab| tab.dirty.get_untracked()))
            })
        });
        spawn_local(ipc::set_dirty(any));
    }

    /// Marks the document of the pane the change came from.
    pub(super) fn mark_dirty(&self, editor_pane: usize) {
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
        drafts::touch(tab, editor_pane);
        self.refresh();
    }

    /// The document now matches its file, so its draft has nothing to restore.
    pub(super) fn mark_clean(&self) {
        let tab = self.tab_untracked();
        tab.dirty.set(false);
        drafts::forget(tab);
        self.sync_dirty();
        self.refresh();
    }

    /// Sends the typing to the pane a click landed in.
    pub(super) fn focus_on(&self, pane: Pane) {
        if let Some(index) = self.index_of(pane) {
            self.focused.set(index);
        }
    }

    pub(super) fn index_of(&self, pane: Pane) -> Option<usize> {
        self.panes
            .with_untracked(|panes| panes.iter().position(|other| other.key == pane.key))
    }

    /// The pane a click or the focus landed in becomes the one in use, so that
    /// the status bar and the shortcuts follow the caret.
    pub(super) fn note_focus(&self, pane: Pane) {
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
    pub(super) fn focus_pane(&self, index: usize) {
        let Some(pane) = self.panes.with_untracked(|panes| panes.get(index).copied()) else {
            return;
        };
        self.focused.set(index);
        editor::focus_pane(pane.editor_pane());
        self.refresh();
    }

    /// Shows the tab at `index` of `pane`, parking the one on screen.
    pub(super) fn switch(&self, pane: Pane, index: usize) {
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
    pub(super) fn show(&self, pane: Pane, tab: Tab) {
        let dirty = tab.dirty.get_untracked();
        let parked = tab.parked.try_update_value(Option::take).flatten();
        // Drawing the document counts as a change, so the mark is set back —
        // and with it the draft, which drawing must not create either.
        editor::restore(pane.editor_pane(), parked);
        tab.dirty.set(dirty);
        if !dirty {
            drafts::forget(tab);
        }
        self.sync_dirty();
        self.refresh();
        editor::focus_pane(pane.editor_pane());
    }

    /// Opens an empty tab, or reuses the shown one when it is untouched.
    pub(super) fn add_tab(&self, pane: Pane) -> Tab {
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

    pub(super) fn new_document(&self) {
        self.add_tab(self.pane_untracked());
        self.status.set(String::new());
    }

    pub(super) fn close(&self, pane: Pane, index: usize) {
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
            // Thrown away on purpose.
            drafts::forget(tab);
            let current = pane.current.get_untracked();
            if pane.tabs.with_untracked(Vec::len) == 1 {
                // The last tab stays, emptied, so there is always a document.
                // It becomes a new tab as far as drafts go.
                tab.id.set(next_id());
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
    pub(super) fn toggle_split(&self) {
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
    pub(super) fn unsplit(&self) {
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

    pub(super) fn open(&self) {
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

    /// Opens what was on screen when the application last stopped. The drafts
    /// come back as unsaved tabs, keeping their numbers so that a second stop
    /// overwrites the same drafts.
    pub(super) fn restore_drafts(&self, drafts: Vec<ipc::Draft>) {
        if drafts.is_empty() {
            return;
        }
        let highest = drafts.iter().map(|draft| draft.id).max().unwrap_or(0);
        NEXT_ID.set(NEXT_ID.get().max(highest + 1));
        let pane = self.pane_untracked();
        for draft in drafts {
            let tab = self.add_tab(pane);
            tab.id.set(draft.id);
            editor::load(&draft.contents);
            tab.path.set(draft.path);
            tab.dirty.set(true);
        }
        self.sync_dirty();
        self.refresh();
        self.status.set("前回の編集内容を復元しました".into());
    }

    pub(super) fn save(&self, force_dialog: bool) {
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
