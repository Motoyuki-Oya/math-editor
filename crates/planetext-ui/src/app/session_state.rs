//! ワークスペースのセッション状態（タブ・ペイン構成、分割比率、未保存ドラフト）の保存と復元。
//! `shell.rs` からセッション永続化の責務を分離します。

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde::{Deserialize, Serialize};

use super::shell::{Shell, Tab};
use crate::editor;
use crate::framework;

/// ワークスペース全体の保存・復元用データ構造
#[derive(Serialize, Deserialize, Clone, Debug)]
pub(super) struct SessionState {
    pub(super) split_ratio: f64,
    pub(super) focused_pane: usize,
    pub(super) panes: Vec<PaneState>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(super) struct PaneState {
    pub(super) current: usize,
    pub(super) tabs: Vec<TabState>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(super) struct TabState {
    pub(super) id: usize,
    #[serde(default)]
    pub(super) untitled_num: Option<usize>,
    pub(super) path: Option<String>,
    pub(super) syntax_override: Option<String>,
    pub(super) dirty: bool,
    #[serde(default)]
    pub(super) modified_lines: Vec<usize>,
}

async fn create_document_from_draft(contents: String) -> Result<framework::OpenedDocument, String> {
    let normalized = contents.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<String> = normalized.split('\n').map(String::from).collect();
    framework::create_document_from_draft(&lines).await
}

impl Shell {
    /// ワークスペース状態（セッション）をディスクに保存します。
    pub(super) fn save_session(&self) {
        if !self.restored.get_untracked() {
            return;
        }
        let split_ratio = self.split_ratio.get_untracked();
        let focused_pane = self.focused.get_untracked();
        let panes = self.panes.get_untracked();
        let mut pane_states = Vec::new();

        for pane in panes {
            let current = pane.current.get_untracked();
            let tabs = pane.tabs.get_untracked();
            let mut tab_states = Vec::new();
            for tab in tabs {
                let modified_lines = editor::doc_modified_lines(tab.id.get_untracked());
                tab_states.push(TabState {
                    id: tab.id.get_untracked(),
                    untitled_num: tab.untitled_num.get_untracked(),
                    path: tab.path.get_untracked(),
                    syntax_override: tab.syntax_override.get_untracked(),
                    dirty: tab.dirty.get_untracked(),
                    modified_lines,
                });
            }
            pane_states.push(PaneState {
                current,
                tabs: tab_states,
            });
        }

        let state = SessionState {
            split_ratio,
            focused_pane,
            panes: pane_states,
        };

        if let Ok(json) = serde_json::to_string(&state) {
            spawn_local(async move {
                framework::save_session_state(&json).await;
            });
        }
    }

    /// ワークスペース全体（ペイン構成・分割比率・開いていたタブ・ドラフト内容・シンタックス）を復元します。
    pub(super) fn restore_workspace(
        &self,
        session_json: Option<String>,
        drafts: Vec<framework::Draft>,
    ) {
        let draft_map: std::collections::HashMap<usize, framework::Draft> =
            drafts.into_iter().map(|d| (d.id, d)).collect();

        let session_state: Option<SessionState> = session_json
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok());

        if let Some(session) = session_state {
            if !session.panes.is_empty() {
                self.split_ratio.set(session.split_ratio);
                let highest_id = session
                    .panes
                    .iter()
                    .flat_map(|p| p.tabs.iter().map(|t| t.id))
                    .max()
                    .unwrap_or(0);
                let draft_highest = draft_map.keys().copied().max().unwrap_or(0);
                self.next_tab_id.set(
                    self.next_tab_id
                        .get_untracked()
                        .max(highest_id.max(draft_highest) + 1),
                );

                let initial_pane = self.pane_untracked();
                let mut created_panes = Vec::new();

                for (p_idx, p_state) in session.panes.into_iter().enumerate() {
                    let pane = if p_idx == 0 {
                        initial_pane
                    } else {
                        let key = self.next_key.get_untracked();
                        self.next_key.set(key + 1);
                        let dummy_tab = self.new_tab();
                        self.new_pane_with_tab(key, dummy_tab)
                    };

                    let mut pane_tabs = Vec::new();
                    for t_state in p_state.tabs {
                        let tab = self
                            .root
                            .with_value(|owner| owner.with(|| Tab::new(t_state.id)));
                        tab.id.set(t_state.id);
                        tab.untitled_num.set(t_state.untitled_num);
                        tab.path.set(t_state.path.clone());
                        tab.syntax_override.set(t_state.syntax_override.clone());

                        let doc_id = t_state.id;
                        let syntax_path = if let Some(ref lang_name) = t_state.syntax_override {
                            let ext = match lang_name.as_str() {
                                "Rust" => "rs",
                                "Python" => "py",
                                "TypeScript" => "ts",
                                "JavaScript" => "js",
                                "HTML" => "html",
                                "CSS" => "css",
                                "JSON" => "json",
                                "TOML" => "toml",
                                "Markdown" => "md",
                                "LaTeX" => "tex",
                                "Kotlin" => "kt",
                                _ => "",
                            };
                            if ext.is_empty() {
                                t_state.path.clone()
                            } else {
                                Some(format!("file.{ext}"))
                            }
                        } else {
                            t_state.path.clone()
                        };
                        editor::set_doc_path(doc_id, syntax_path);

                        if let Some(draft) = draft_map.get(&t_state.id) {
                            tab.dirty.set(!draft.clean);
                            if t_state.path.is_some() {
                                let draft_id = draft.id.to_string();
                                let tab_copy = tab;
                                let shell_copy = *self;
                                spawn_local(async move {
                                    match framework::open_draft(&draft_id).await {
                                        Ok(doc) => {
                                            if tab_copy.doc.get_untracked().is_some() {
                                                framework::close_document(doc.handle).await;
                                            } else {
                                                tab_copy.dirty.set(!doc.clean);
                                                shell_copy.sync_dirty();
                                                tab_copy.doc.set(Some(doc.handle));
                                                tab_copy.bytes.set(doc.bytes);
                                                tab_copy.encoding.set(doc.encoding);
                                                tab_copy.line_ending.set(doc.line_ending);
                                                editor::set_doc_file_size(doc_id, Some(doc.bytes));
                                                editor::load_sparse_doc(doc_id, doc.line_count);
                                                editor::redraw_doc(doc_id, None);
                                                shell_copy.save_session();
                                                if doc.line_count.is_none() {
                                                    let handle = doc.handle;
                                                    spawn_local(async move {
                                                        if let Ok(count) =
                                                            framework::finish_document(handle).await
                                                        {
                                                            shell_copy
                                                                .document_scanned(handle, count);
                                                        }
                                                    });
                                                }
                                            }
                                        }
                                        Err(error) => shell_copy.status.set(error),
                                    }
                                });
                            } else {
                                editor::load_doc_contents(draft.id, &draft.contents);
                                if !t_state.modified_lines.is_empty() {
                                    editor::set_doc_modified_lines(
                                        draft.id,
                                        t_state.modified_lines,
                                    );
                                } else {
                                    editor::mark_doc_all_modified(draft.id);
                                }

                                let tab_copy = tab;
                                let draft_contents = draft.contents.clone();
                                let shell_copy = *self;
                                spawn_local(async move {
                                    match create_document_from_draft(draft_contents).await {
                                        Ok(opened) => {
                                            if tab_copy.doc.get_untracked().is_some() {
                                                framework::close_document(opened.handle).await;
                                            } else {
                                                tab_copy.doc.set(Some(opened.handle));
                                                tab_copy.encoding.set(opened.encoding);
                                                tab_copy.line_ending.set(opened.line_ending);
                                            }
                                        }
                                        Err(error) => shell_copy.status.set(error),
                                    }
                                });
                            }
                        } else if let Some(ref path) = t_state.path {
                            let path_clone = path.clone();
                            let tab_copy = tab;
                            let shell_copy = *self;
                            spawn_local(async move {
                                match framework::open_document(&path_clone).await {
                                    Ok(doc) => {
                                        if tab_copy.doc.get_untracked().is_some() {
                                            framework::close_document(doc.handle).await;
                                        } else {
                                            tab_copy.doc.set(Some(doc.handle));
                                            tab_copy.bytes.set(doc.bytes);
                                            tab_copy.encoding.set(doc.encoding);
                                            tab_copy.line_ending.set(doc.line_ending);
                                            editor::set_doc_file_size(doc_id, Some(doc.bytes));
                                            editor::load_sparse_doc(doc_id, doc.line_count);
                                            editor::redraw_doc(doc_id, None);
                                            shell_copy.save_session();
                                            if doc.line_count.is_none() {
                                                let handle = doc.handle;
                                                spawn_local(async move {
                                                    if let Ok(count) =
                                                        framework::finish_document(handle).await
                                                    {
                                                        shell_copy.document_scanned(handle, count);
                                                    }
                                                });
                                            }
                                        }
                                    }
                                    Err(error) => shell_copy.status.set(error),
                                }
                            });
                        } else {
                            tab.assign_document();
                        }

                        pane_tabs.push(tab);
                    }

                    if !pane_tabs.is_empty() {
                        let active_idx = p_state.current.min(pane_tabs.len() - 1);
                        pane.tabs.set(pane_tabs.clone());
                        pane.current.set(active_idx);
                        editor::bind_doc(
                            pane.editor_pane(),
                            pane_tabs[active_idx].id.get_untracked(),
                        );
                    }
                    created_panes.push(pane);
                }

                if !created_panes.is_empty() {
                    self.panes.set(created_panes);
                    let active_pane = session
                        .focused_pane
                        .min(self.panes.with_untracked(Vec::len) - 1);
                    self.focused.set(active_pane);
                    let pane_obj = self.panes.with_untracked(|p| p[active_pane]);
                    editor::focus_pane(pane_obj.editor_pane());
                }

                editor::redraw_all();
                self.restored.set(true);
                self.sync_dirty();
                self.refresh();
                super::menu::update_menu_state();
                self.status.set("前回のワークスペースを復元しました".into());
                return;
            }
        }

        if !draft_map.is_empty() {
            let drafts_vec: Vec<framework::Draft> = draft_map.into_values().collect();
            self.restore_drafts(drafts_vec);
            return;
        }

        self.restored.set(true);
        self.save_session();
    }

    /// アプリケーションが最後に停止したときに画面に表示されていたものを開きます。ドラフトは未保存のタブとして返され、番号が保持されるため、2 番目のストップで同じドラフトが上書きされます。
    pub(super) fn restore_drafts(&self, drafts: Vec<framework::Draft>) {
        if drafts.is_empty() {
            return;
        }
        let highest = drafts.iter().map(|draft| draft.id).max().unwrap_or(0);
        self.next_tab_id
            .set(self.next_tab_id.get_untracked().max(highest + 1));
        let pane = self.pane_untracked();
        let mut tabs = Vec::new();
        for (i, draft) in drafts.into_iter().enumerate() {
            let tab = self
                .root
                .with_value(|owner| owner.with(|| Tab::new(draft.id)));
            tab.id.set(draft.id);
            if draft.path.is_none() {
                tab.untitled_num.set(Some(i + 1));
            } else {
                tab.untitled_num.set(None);
            }
            tab.path.set(draft.path.clone());
            tab.dirty.set(!draft.clean);
            editor::set_doc_path(draft.id, draft.path.clone());
            if draft.path.is_some() {
                let draft_id = draft.id.to_string();
                let doc_id = draft.id;
                let tab_copy = tab;
                let shell_copy = *self;
                spawn_local(async move {
                    match framework::open_draft(&draft_id).await {
                        Ok(doc) => {
                            if tab_copy.doc.get_untracked().is_some() {
                                framework::close_document(doc.handle).await;
                            } else {
                                tab_copy.dirty.set(!doc.clean);
                                shell_copy.sync_dirty();
                                tab_copy.doc.set(Some(doc.handle));
                                tab_copy.bytes.set(doc.bytes);
                                tab_copy.encoding.set(doc.encoding);
                                tab_copy.line_ending.set(doc.line_ending);
                                editor::set_doc_file_size(doc_id, Some(doc.bytes));
                                editor::load_sparse_doc(doc_id, doc.line_count);
                                editor::redraw_doc(doc_id, None);
                                shell_copy.save_session();
                                if doc.line_count.is_none() {
                                    let handle = doc.handle;
                                    spawn_local(async move {
                                        if let Ok(count) = framework::finish_document(handle).await
                                        {
                                            shell_copy.document_scanned(handle, count);
                                        }
                                    });
                                }
                            }
                        }
                        Err(error) => shell_copy.status.set(error),
                    }
                });
            } else {
                editor::load_doc_contents(draft.id, &draft.contents);
                editor::mark_doc_all_modified(draft.id);

                let tab_copy = tab;
                let draft_contents = draft.contents.clone();
                let shell_copy = *self;
                spawn_local(async move {
                    match create_document_from_draft(draft_contents).await {
                        Ok(opened) => {
                            if tab_copy.doc.get_untracked().is_some() {
                                framework::close_document(opened.handle).await;
                            } else {
                                tab_copy.doc.set(Some(opened.handle));
                                tab_copy.encoding.set(opened.encoding);
                                tab_copy.line_ending.set(opened.line_ending);
                            }
                        }
                        Err(error) => shell_copy.status.set(error),
                    }
                });
            }
            tabs.push(tab);
        }
        if !tabs.is_empty() {
            pane.tabs.set(tabs.clone());
            pane.current.set(0);
            editor::bind_doc(pane.editor_pane(), tabs[0].id.get_untracked());
        }
        editor::redraw_all();
        self.restored.set(true);
        self.sync_dirty();
        self.refresh();
        super::menu::update_menu_state();
        self.save_session();
        self.status.set("前回の編集内容を復元しました".into());
    }
}
