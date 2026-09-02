//! WebView側接続コードの共通界面。Host固有の詳細はhost.rsだけが知る。

mod host;

#[allow(unused_imports)]
pub(crate) use host::{
    cancel_search, close_document, copy_range, create_document, create_document_from_draft,
    estimate_matches, file_size, finish_document, lines_containing, open_document, read_drafts,
    read_lines, read_session_state, read_settings, read_tail, remove_draft,
    reopen_document_encoding, replace_lines, save_document, save_draft, save_session_state,
    search_document, set_dirty, set_document_encoding, set_document_line_ending, undo_lines,
    write_settings, Draft, EditApplied, OpenedDocument, ReadLines,
};

pub(crate) type GuiError = String;

pub(crate) struct MenuState {
    pub(crate) wrap: bool,
    pub(crate) line_numbers: bool,
    pub(crate) show_whitespace: bool,
    pub(crate) split: bool,
}

pub(crate) enum GuiEvent {
    MenuSelected(String),
}

pub(crate) trait GuiFramework {
    async fn pick_open_file(&self) -> Result<Option<String>, GuiError>;
    async fn pick_save_file(&self, default_name: &str) -> Result<Option<String>, GuiError>;
    async fn confirm(&self, message: &str) -> Result<bool, GuiError>;
    async fn set_menu(&self, state: MenuState) -> Result<(), GuiError>;
    async fn open_external(&self, target: &str) -> Result<(), GuiError>;
    async fn ready(&self) -> Result<(), GuiError>;
    fn on_event(&self, handler: Box<dyn Fn(GuiEvent) + 'static>) -> Result<(), GuiError>;
}

pub(crate) fn gui() -> &'static impl GuiFramework {
    &host::GUI
}
