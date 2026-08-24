//! アプリケーションの状態: ペイン、タブ、それらに表示されるファイル、およびそれらを開く、保存する、閉じる際の動作。

use leptos::prelude::*;
use leptos::reactive::owner::{LocalStorage, Owner};
use leptos::task::spawn_local;

use super::drafts;
use super::sync;
use crate::editor;
use crate::ipc;

const UNTITLED: &str = "無題";

/// これより大きい文書は下書き（自動控え）を書かない。下書きは全文を
/// 書き出すので、巨大なファイルでは一時停止のたびに数百 MB を書くことになる。
const LARGE_BYTES: usize = 5_000_000;

thread_local! {
    /// 次のタブに名前を付けます。タブの番号はそのドラフトの名前でもあるため、タブ自体よりも存続する必要があります。復元されたドラフトではその番号が保持されます。
    static NEXT_ID: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn next_id() -> usize {
    let id = NEXT_ID.get();
    NEXT_ID.set(id + 1);
    id
}

/// 開いているファイルが 1 つあります。ドキュメント自体は、タブが表示されている間はエディター内に存在し、別のタブが表示されている間はここに保留されます。
#[derive(Clone, Copy)]
pub(super) struct Tab {
    /// ディスク上のこのタブのドラフトに名前を付けます。
    pub(super) id: RwSignal<usize>,
    pub(super) path: RwSignal<Option<String>>,
    pub(super) dirty: RwSignal<bool>,
    /// 下書きを書くには大きすぎる文書。
    pub(super) large: RwSignal<bool>,
    /// このタブの文書の本体を指す、ネイティブ側ストアの取っ手。
    /// 新しいタブでは作成が非同期に届くまで `None`。
    pub(super) doc: RwSignal<Option<u64>>,
    pub(super) parked: StoredValue<Option<editor::Parked>, LocalStorage>,
}

impl Tab {
    pub(super) fn new() -> Tab {
        Tab {
            id: RwSignal::new(next_id()),
            path: RwSignal::new(None),
            dirty: RwSignal::new(false),
            large: RwSignal::new(false),
            doc: RwSignal::new(None),
            parked: StoredValue::new_local(None),
        }
    }

    /// ネイティブ側に空の文書を作り、届いたら取っ手を持つ。
    pub(super) fn assign_document(&self) {
        let tab = *self;
        spawn_local(async move {
            if let Some(doc) = ipc::create_document().await {
                // 取っ手が既にある（開くが先に済んだ）なら、作った文書は要らない。
                if tab.doc.get_untracked().is_some() {
                    ipc::close_document(doc.handle).await;
                } else {
                    tab.doc.set(Some(doc.handle));
                }
            }
        });
    }

    /// タブが手放す文書。閉じるときに呼ぶ。
    pub(super) fn release_document(&self) {
        if let Some(handle) = self.doc.get_untracked() {
            self.doc.set(None);
            spawn_local(async move { ipc::close_document(handle).await });
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

/// ドキュメントの 1 つのビュー: 独自のタブ ストリップと独自のエディター。分割ビューを使用すると、複数のビューが表示されます。
#[derive(Clone, Copy)]
pub(super) struct Pane {
    /// 要素が存在すると、エディター コアでこのペインに名前を付けます。
    pub(super) editor: StoredValue<Option<usize>>,
    pub(super) tabs: RwSignal<Vec<Tab>>,
    pub(super) current: RwSignal<usize>,
    /// このペインで構造パレットが表示されているかどうか。
    pub(super) palette: RwSignal<bool>,
    /// レンダリング間でペインの要素を保持します。
    pub(super) key: usize,
}

impl Pane {
    pub(super) fn new(key: usize) -> Pane {
        let tab = Tab::new();
        tab.assign_document();
        Pane {
            editor: StoredValue::new(None),
            tabs: RwSignal::new(vec![tab]),
            current: RwSignal::new(0),
            palette: RwSignal::new(false),
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

    /// 表示されているドキュメントを画面から外し、タブを付けたままにします。
    /// 下書きは文書の本体から書かれるので、画面から外れても書けます。
    pub(super) fn park(&self) {
        let pane = self.editor_pane();
        let tab = self.tab_untracked();
        tab.parked.set_value(editor::park(pane));
    }
}

#[derive(Clone, Copy)]
pub(super) struct Shell {
    /// ペインとタブはこの所有者の下で作成され、各ペインの存続​​期間を超えて存続するため、ペインを閉じても、渡されたドキュメントが削除されることはありません。
    pub(super) root: StoredValue<Owner, LocalStorage>,
    pub(super) panes: RwSignal<Vec<Pane>>,
    /// どのペインが入力を必要とするか。
    pub(super) focused: RwSignal<usize>,
    pub(super) next_key: RwSignal<usize>,
    pub(super) status: RwSignal<String>,
    pub(super) stats: RwSignal<(usize, usize)>,
    pub(super) searching: RwSignal<bool>,
    /// 設定が画面上に表示されるかどうか。
    pub(super) preferences: RwSignal<bool>,
    /// カーソルが画面上に表示されると、それを待機する検索バーのフィールド。
    pub(super) find_focus: RwSignal<Option<Field>>,
    /// ドラッグ中のタブ情報（移動元、マウス位置、ドラッグ中フラグ、ホバー中ターゲット）。
    pub(super) tab_drag: RwSignal<Option<TabDragState>>,
    /// 左右分割ペインの幅比率（0.1 〜 0.9、既定 0.5）。
    pub(super) split_ratio: RwSignal<f64>,
    /// 分割線のドラッグ中フラグ。
    pub(super) resizing_split: RwSignal<bool>,
}

/// ドラッグ中のタブ状態。
#[derive(Clone, Debug, PartialEq)]
pub(super) struct TabDragState {
    pub(super) src_pane_key: usize,
    pub(super) src_tab_index: usize,
    pub(super) tab_name: String,
    pub(super) start_x: f64,
    pub(super) start_y: f64,
    pub(super) current_x: f64,
    pub(super) current_y: f64,
    pub(super) is_dragging: bool,
    pub(super) drop_target: Option<DropTarget>,
}

/// ドロップ先のペインと挿入インデックス。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct DropTarget {
    pub(super) pane_key: usize,
    pub(super) index: usize,
}

/// 検索バーのフィールド。ショートカットで検索できるように名前が付けられています。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Field {
    Query,
    Replacement,
}

impl Shell {
    /// カーソルが「フィールド」にある状態で検索バーを開きます。これは、Ctrl+F および Ctrl+R で行われることです。
    pub(super) fn find(&self, field: Field) {
        self.searching.set(true);
        self.find_focus.set(Some(field));
    }

    pub(super) fn new_tab(&self) -> Tab {
        let tab = self.root.with_value(|owner| owner.with(Tab::new));
        tab.assign_document();
        tab
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

    /// ウィンドウを閉じると作業が失われるかどうかをネイティブ側に伝えます。
    pub(super) fn sync_dirty(&self) {
        let any = self.panes.with_untracked(|panes| {
            panes.iter().any(|pane| {
                pane.tabs
                    .with_untracked(|tabs| tabs.iter().any(|tab| tab.dirty.get_untracked()))
            })
        });
        spawn_local(ipc::set_dirty(any));
    }

    /// エディターのペインに表示されているタブ。
    pub(super) fn tab_of(&self, editor_pane: usize) -> Option<Tab> {
        self.panes.with_untracked(|panes| {
            panes
                .iter()
                .find(|pane| pane.editor.get_value() == Some(editor_pane))
                .map(|pane| pane.tab_untracked())
        })
    }

    /// 届いた行をタブの文書へ入れます。タブが画面上ならそのペインへ、
    /// 駐車中ならその文書へ。
    pub(super) fn feed(&self, tab: Tab, from: usize, lines: &[String]) {
        if tab.parked.with_value(Option::is_some) {
            tab.parked.update_value(|parked| {
                if let Some(parked) = parked {
                    parked.feed(from, lines);
                }
            });
            return;
        }
        if let Some(pane) = self.pane_showing(tab) {
            editor::feed_pane(pane.editor_pane(), from, lines);
        }
    }

    /// 文書の本体が巻き戻ったのを、タブがどこにいても手元へ映します。
    pub(super) fn apply_restored(
        &self,
        tab: Tab,
        state: &str,
        touched_from: usize,
        line_count: usize,
    ) {
        if tab.parked.with_value(Option::is_some) {
            tab.parked.update_value(|parked| {
                if let Some(parked) = parked {
                    parked.apply_restored(state, touched_from, line_count);
                }
            });
            return;
        }
        if let Some(pane) = self.pane_showing(tab) {
            editor::apply_restored(pane.editor_pane(), state, touched_from, line_count);
        }
    }

    pub(super) fn pane_showing(&self, tab: Tab) -> Option<Pane> {
        let id = tab.id.get_untracked();
        self.panes.with_untracked(|panes| {
            panes
                .iter()
                .find(|pane| pane.tab_untracked().id.get_untracked() == id)
                .copied()
        })
    }

    /// 変更元のペインのドキュメントをマークし、たまった編集を本体へ送ります。
    pub(super) fn mark_dirty(&self, editor_pane: usize) {
        sync::flush(*self, editor_pane);
        let pane = self
            .panes
            .with_untracked(|panes| {
                panes
                    .iter()
                    .find(|pane| pane.editor.get_value() == Some(editor_pane))
                    .copied()
            })
            .unwrap_or_else(|| self.pane_untracked());
        self.mark_dirty_tab(pane.tab_untracked());
    }

    pub(super) fn mark_dirty_tab(&self, tab: Tab) {
        if !tab.dirty.get_untracked() {
            tab.dirty.set(true);
            self.sync_dirty();
        }
        drafts::touch(tab);
        self.refresh();
    }

    /// ドキュメントがそのファイルと一致するようになったので、そのドラフトには復元するものは何もありません。
    pub(super) fn mark_clean(&self) {
        self.mark_clean_tab(self.tab_untracked());
    }

    pub(super) fn mark_clean_tab(&self, tab: Tab) {
        tab.dirty.set(false);
        drafts::forget(tab);
        if let Some(pane) = self.pane_showing(tab) {
            editor::clear_modified(pane.editor_pane());
        }
        self.sync_dirty();
        self.refresh();
    }

    /// クリックされたペインに入力内容を送信します。
    pub(super) fn focus_on(&self, pane: Pane) {
        if let Some(index) = self.index_of(pane) {
            self.focused.set(index);
        }
    }

    pub(super) fn index_of(&self, pane: Pane) -> Option<usize> {
        self.panes
            .with_untracked(|panes| panes.iter().position(|other| other.key == pane.key))
    }

    /// クリックまたはフォーカスが置かれたペインが使用中のペインになり、ステータス バーとショートカットがキャレットに従います。
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

    /// 入力内容をペインに送信します。
    pub(super) fn focus_pane(&self, index: usize) {
        let Some(pane) = self.panes.with_untracked(|panes| panes.get(index).copied()) else {
            return;
        };
        self.focused.set(index);
        editor::focus_pane(pane.editor_pane());
        self.refresh();
    }

    /// `pane` の `index` にあるタブを表示し、そのタブを画面上に駐車します。
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

    /// タブのドキュメントを `pane` の画面に置き、未保存のマークを保持します。
    pub(super) fn show(&self, pane: Pane, tab: Tab) {
        let dirty = tab.dirty.get_untracked();
        let parked = tab.parked.try_update_value(Option::take).flatten();
        // 文書を描画することは変更としてカウントされるため、マークは元に戻されます。また、それとともに下書きも設定され、その描画も作成してはなりません。
        editor::restore(pane.editor_pane(), parked);
        tab.dirty.set(dirty);
        if !dirty {
            drafts::forget(tab);
        }
        self.sync_dirty();
        self.refresh();
        editor::focus_pane(pane.editor_pane());
    }

    /// 空のタブを開くか、表示されているタブが未操作の場合は再利用します。
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
            // わざと捨てた。
            drafts::forget(tab);
            tab.release_document();
            let current = pane.current.get_untracked();
            if pane.tabs.with_untracked(Vec::len) == 1 {
                // 最後のタブは空のままなので、常にドキュメントが存在します。下書きに関しては、新しいタブになります。
                tab.id.set(next_id());
                tab.path.set(None);
                tab.large.set(false);
                tab.assign_document();
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

    /// タブを並べ替えるか、別のペインへ移動します。
    pub(super) fn move_tab(
        &self,
        src_pane_key: usize,
        src_tab_idx: usize,
        dst_pane: Pane,
        dst_tab_idx: usize,
    ) {
        let Some(src_pane) = self
            .panes
            .with_untracked(|panes| panes.iter().find(|p| p.key == src_pane_key).copied())
        else {
            return;
        };

        if src_pane.key == dst_pane.key {
            // 同一ペイン内での並べ替え
            let tab_count = src_pane.tabs.with_untracked(Vec::len);
            if src_tab_idx >= tab_count {
                return;
            }
            let insert_pos = if src_tab_idx < dst_tab_idx {
                dst_tab_idx.saturating_sub(1)
            } else {
                dst_tab_idx
            }
            .min(tab_count.saturating_sub(1));

            if src_tab_idx == insert_pos {
                return;
            }

            let active_tab_id = src_pane.tab_untracked().id.get_untracked();
            src_pane.tabs.update(|tabs| {
                let tab = tabs.remove(src_tab_idx);
                tabs.insert(insert_pos, tab);
            });
            // アクティブなタブの新しい位置を特定して current を更新
            if let Some(new_curr) = src_pane.tabs.with_untracked(|tabs| {
                tabs.iter()
                    .position(|t| t.id.get_untracked() == active_tab_id)
            }) {
                src_pane.current.set(new_curr);
            }
        } else {
            // 分割ペイン間でのタブ移動
            let Some(tab) = src_pane
                .tabs
                .with_untracked(|tabs| tabs.get(src_tab_idx).copied())
            else {
                return;
            };

            let src_current = src_pane.current.get_untracked();
            if src_current == src_tab_idx {
                // 移動元で表示中のタブを移動する場合、エディタの状態を park に退避
                src_pane.park();
            }

            src_pane.tabs.update(|tabs| {
                if src_tab_idx < tabs.len() {
                    tabs.remove(src_tab_idx);
                }
            });

            if src_pane.tabs.with_untracked(Vec::is_empty) {
                // 元ペインが空になる場合、空の無題タブを新規作成してペインを維持
                let new_tab = self.new_tab();
                src_pane.tabs.set(vec![new_tab]);
                src_pane.current.set(0);
                self.show(src_pane, new_tab);
            } else if src_current == src_tab_idx {
                let last = src_pane.tabs.with_untracked(|tabs| tabs.len() - 1);
                let next_idx = src_tab_idx.min(last);
                src_pane.current.set(next_idx);
                let next_tab = src_pane.tabs.with_untracked(|tabs| tabs[next_idx]);
                self.show(src_pane, next_tab);
            } else {
                let last = src_pane.tabs.with_untracked(|tabs| tabs.len() - 1);
                if src_tab_idx < src_current {
                    src_pane.current.set(src_current - 1);
                } else {
                    src_pane.current.set(src_current.min(last));
                }
            }

            // 宛先ペインへタブを挿入
            let dst_count = dst_pane.tabs.with_untracked(Vec::len);
            let insert_idx = dst_tab_idx.min(dst_count);
            dst_pane.tabs.update(|tabs| {
                tabs.insert(insert_idx, tab);
            });

            // 移動したタブを宛先ペインでアクティブにして表示
            dst_pane.park();
            dst_pane.current.set(insert_idx);
            self.show(dst_pane, tab);
            self.focus_on(dst_pane);
            self.sync_dirty();
            self.refresh();
        }
    }

    /// 表示されているペインの横にペインを追加するか、表示されているペインを削除します。
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

    /// ペインの使用を維持し、他のペインを削除します。タブが移動するため、ドキュメントは閉じられません。
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
        // タブが最初に移動します。タブが移動するまでは、タブの元のペインがタブを所有します。
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
            match ipc::open_document(&path).await {
                Ok(doc) => {
                    let pane = shell.pane_untracked();
                    let tab = shell.add_tab(pane);
                    // 空のタブを使い回したときは、そのタブの空文書を手放して
                    // 開いた文書の取っ手に替える。
                    tab.release_document();
                    tab.doc.set(Some(doc.handle));
                    tab.large.set(doc.bytes > LARGE_BYTES);
                    // 行は見えた場所から取り寄せられる。最初の描き直しが
                    // 見えている窓を要求する。
                    editor::load_pending(doc.line_count);
                    tab.path.set(Some(path));
                    shell.status.set("開きました".into());
                    shell.mark_clean();
                    // 行数はバックグラウンドで走査中。確定したら手元へ合わせる。
                    let handle = doc.handle;
                    let editor_pane = pane.editor_pane();
                    spawn_local(async move {
                        match ipc::finish_document(handle).await {
                            Ok(count) => {
                                editor::set_line_count(editor_pane, count);
                                shell.refresh();
                            }
                            Err(error) => shell.status.set(error),
                        }
                    });
                }
                Err(error) => shell.status.set(error),
            }
        });
    }

    /// アプリケーションが最後に停止したときに画面に表示されていたものを開きます。ドラフトは未保存のタブとして返され、番号が保持されるため、2 番目のストップで同じドラフトが上書きされます。
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

    /// 保存は文書の本体がディスクへ直接行います。たまった編集と同じ列に並ぶ
    /// ので、送信中の編集を追い越しません。
    pub(super) fn save(&self, force_dialog: bool) {
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
            sync::save(tab, path);
        });
    }
}
