//! 文書エンジン。GUIフレームワークを知らない。

mod document;
mod edit_buffers;
mod operation_log;
mod persistence;
mod piece_tree;
mod search;
mod search_index;
mod source;
#[cfg(test)]
mod store;

use document::Document;
pub use search::CompiledQuery;
use search::{ScanHit, SearchSpec};
use source::{FileEncoding, LineEnding, ScanIndex};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub struct Application {
    state: Arc<ApplicationState>,
}

#[derive(Default)]
struct ApplicationState {
    dirty: Mutex<bool>,
    /// 開いている文書の本体。webview は行の窓だけを取り寄せ、編集は
    /// 行範囲の置き換えとして届く。タブが閉じられると手放す。
    docs: Mutex<HashMap<u64, Document>>,
    /// 文書ごとの検索世代。値が変われば走査スレッドは古い検索を中止する。
    searches: Mutex<HashMap<u64, Arc<AtomicU64>>>,
    next_document: Mutex<u64>,
}

/// 開き方の答え: 文書の取っ手と、行数と大きさ、文字コード、改行コード、リビジョン。
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct OpenedDocument {
    pub handle: u64,
    pub line_count: usize,
    pub bytes: usize,
    pub encoding: String,
    pub line_ending: String,
    pub revision: u64,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReopenedDocument {
    pub line_count: usize,
    pub encoding: String,
    pub line_ending: String,
    pub revision: u64,
}

/// 元に戻す・やり直すの結果。`state` は frontend が預けた控えそのもの。
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct RestoredLines {
    pub state: String,
    pub touched_from: usize,
    pub line_count: usize,
    pub clean: bool,
    pub revision: u64,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ReadLines {
    pub lines: Vec<String>,
    pub from: usize,
    pub revision: u64,
}

impl PartialEq<Vec<&str>> for ReadLines {
    fn eq(&self, other: &Vec<&str>) -> bool {
        self.lines
            .iter()
            .map(String::as_str)
            .eq(other.iter().copied())
    }
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct EditApplied {
    pub line_count: usize,
    pub revision: u64,
}

#[derive(serde::Serialize)]
pub struct SearchPage {
    hits: Vec<ScanHit>,
    scanned_to: usize,
    cancelled: bool,
}

/// 下書きファイルは最初の行にドキュメントのパスがあり、その後にドキュメント自体が含まれているため、復元されたドラフトではそれがどのファイルに属しているかがわかります。
#[derive(serde::Serialize)]
pub struct Draft {
    id: String,
    path: Option<String>,
    contents: String,
}

pub struct FinishDocumentJob {
    application: Application,
    handle: u64,
    index: Option<Arc<ScanIndex>>,
}

impl FinishDocumentJob {
    pub fn poll(&self) -> Result<Option<usize>, String> {
        if let Some(index) = &self.index {
            if index.status()?.is_none() {
                return Ok(None);
            }
        }
        self.application.with_doc(self.handle, |doc| {
            doc.confirm_scan();
            Ok(())
        })?;
        self.application
            .with_doc(self.handle, |doc| Ok(doc.line_count()))
            .map(Some)
    }
}

pub struct SearchJob {
    application: Application,
    handle: u64,
    snapshot: Document,
    query: CompiledQuery,
    from: usize,
    end: usize,
    after_col: Option<usize>,
    generation: Arc<AtomicU64>,
    ticket: u64,
}

impl SearchJob {
    pub fn run(mut self) -> Result<SearchPage, String> {
        let found = self.snapshot.search_candidates(
            SearchSpec {
                query: &self.query,
                from: self.from,
                end: self.end,
                after_col: self.after_col,
            },
            &|| self.generation.load(Ordering::Relaxed) != self.ticket,
        )?;
        let mapped_hits = self
            .application
            .with_doc(self.handle, |doc| {
                Ok(doc.map_search_hits(&self.snapshot, found.hits))
            })
            .unwrap_or_default();

        Ok(SearchPage {
            hits: mapped_hits,
            scanned_to: found.scanned_to,
            cancelled: found.cancelled,
        })
    }
}

pub struct GlobalShortcutSettings {
    pub enabled: bool,
    pub key: String,
}

pub enum GuiEvent {
    CloseRequested,
    TraySelected(TrayAction),
    GlobalShortcut(String),
    SecondInstance,
}

pub enum TrayAction {
    Open,
    Quit,
}

pub enum GuiAction {
    ShowWindow,
    HideWindow,
    ToggleWindow,
    Exit,
    ConfirmExit,
}

pub fn global_shortcut_settings(settings_text: &str) -> GlobalShortcutSettings {
    let enabled = !settings_text.lines().any(|line| {
        let Some((key, value)) = line.split_once('=') else {
            return false;
        };
        key.trim() == "global_shortcut" && value.trim() == "false"
    });
    let key = settings_text
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once('=')?;
            if key.trim() == "global_shortcut_key" {
                let value = value.trim().trim_matches('"').trim_matches('\'');
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
            None
        })
        .unwrap_or_else(|| "Ctrl+Shift+M".to_string());
    GlobalShortcutSettings { enabled, key }
}

pub struct SettingsWrite {
    path: PathBuf,
    contents: String,
}

impl SettingsWrite {
    pub fn contents(&self) -> &str {
        &self.contents
    }

    pub fn write(self) -> Result<(), String> {
        std::fs::write(&self.path, self.contents)
            .map_err(|e| format!("設定を保存できませんでした: {e}"))
    }
}

impl Application {
    /// 取っ手の文書へロックの中で触る。閉じられた文書は一律に断る。
    fn with_doc<T>(
        &self,
        handle: u64,
        f: impl FnOnce(&mut Document) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut docs = self.state.docs.lock().unwrap();
        let doc = docs
            .get_mut(&handle)
            .ok_or_else(|| "文書はもう閉じられています".to_string())?;
        f(doc)
    }

    fn adopt(&self, doc: Document) -> OpenedDocument {
        let encoding = doc.encoding().label().to_string();
        let line_ending = doc.line_ending().label().to_string();
        let revision = doc.revision();
        // 走査未完了の間は行数は未確定（0）として返す。初期スキャンチャンク行数は漏らさない。
        let line_count = if doc.pending_source.is_some() {
            0
        } else {
            doc.line_count()
        };
        let opened = OpenedDocument {
            handle: {
                let mut next = self.state.next_document.lock().unwrap();
                *next += 1;
                *next
            },
            line_count,
            bytes: doc.bytes(),
            encoding,
            line_ending,
            revision,
        };
        self.state.docs.lock().unwrap().insert(opened.handle, doc);
        self.state
            .searches
            .lock()
            .unwrap()
            .insert(opened.handle, Arc::new(AtomicU64::new(0)));
        opened
    }

    pub fn open_document(&self, path: String) -> Result<OpenedDocument, String> {
        let (mut doc, scan) = Document::open(&path)?;
        let bg_index = doc.take_background_index();
        let opened = self.adopt(doc);
        if let Some(scan) = scan {
            std::thread::spawn(move || {
                let _ = scan.run();
            });
        }
        if let Some(bg_index) = bg_index {
            std::thread::spawn(move || {
                bg_index.run();
            });
        }
        Ok(opened)
    }

    pub fn reopen_document_encoding(
        &self,
        handle: u64,
        encoding: String,
    ) -> Result<ReopenedDocument, String> {
        let enc = FileEncoding::from_label(&encoding)
            .ok_or_else(|| format!("未知の文字コードです: {encoding}"))?;
        let (line_count, enc_label, line_ending, revision, scan, bg_index) =
            self.with_doc(handle, |doc| {
                let scan = doc.reopen_with_encoding(enc)?;
                let bg_index = doc.take_background_index();
                let line_count = if scan.is_some() { 0 } else { doc.line_count() };
                Ok((
                    line_count,
                    doc.encoding().label().to_string(),
                    doc.line_ending().label().to_string(),
                    doc.revision(),
                    scan,
                    bg_index,
                ))
            })?;
        if let Some(scan) = scan {
            std::thread::spawn(move || {
                let _ = scan.run();
            });
        }
        if let Some(bg_index) = bg_index {
            std::thread::spawn(move || {
                bg_index.run();
            });
        }
        Ok(ReopenedDocument {
            line_count,
            encoding: enc_label,
            line_ending,
            revision,
        })
    }

    pub fn set_document_encoding(&self, handle: u64, encoding: String) -> Result<(), String> {
        let enc = FileEncoding::from_label(&encoding)
            .ok_or_else(|| format!("未知の文字コードです: {encoding}"))?;
        self.with_doc(handle, |doc| {
            doc.set_encoding(enc);
            Ok(())
        })
    }

    pub fn set_document_line_ending(&self, handle: u64, line_ending: String) -> Result<(), String> {
        let le = LineEnding::from_label(&line_ending)
            .ok_or_else(|| format!("未知の改行コードです: {line_ending}"))?;
        self.with_doc(handle, |doc| {
            doc.set_line_ending(le);
            Ok(())
        })
    }

    pub fn finish_document(&self, handle: u64) -> Result<FinishDocumentJob, String> {
        let index = self.with_doc(handle, |doc| Ok(doc.scan_index()))?;
        Ok(FinishDocumentJob {
            application: self.clone(),
            handle,
            index,
        })
    }

    pub fn create_document(&self) -> OpenedDocument {
        self.adopt(Document::empty())
    }

    pub fn create_document_from_draft(&self, lines: Vec<String>) -> Result<OpenedDocument, String> {
        let doc = Document::from_draft(lines);
        Ok(self.adopt(doc))
    }

    pub fn read_lines(&self, handle: u64, from: usize, count: usize) -> Result<ReadLines, String> {
        self.with_doc(handle, |doc| {
            let lines = doc.read(from, count)?;
            Ok(ReadLines {
                lines,
                from,
                revision: doc.revision(),
            })
        })
    }

    pub fn read_tail(&self, handle: u64, count: usize) -> Result<ReadLines, String> {
        self.with_doc(handle, |doc| {
            let lines = doc.read_tail(count)?;
            let from = doc.line_count().saturating_sub(lines.len());
            Ok(ReadLines {
                lines,
                from,
                revision: doc.revision(),
            })
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn replace_lines(
        &self,
        handle: u64,
        from: usize,
        to: usize,
        lines: Vec<String>,
        group: u64,
        before: String,
        after: String,
    ) -> Result<EditApplied, String> {
        self.with_doc(handle, |doc| {
            let line_count = doc.replace(from, to, lines, group, &before, &after)?;
            Ok(EditApplied {
                line_count,
                revision: doc.revision(),
            })
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn replace_lines_with_base(
        &self,
        handle: u64,
        base_revision: u64,
        from: usize,
        to: usize,
        lines: Vec<String>,
        group: u64,
        before: String,
        after: String,
    ) -> Result<EditApplied, String> {
        self.with_doc(handle, |doc| {
            let line_count =
                doc.replace_with_base(base_revision, from, to, lines, group, &before, &after)?;
            Ok(EditApplied {
                line_count,
                revision: doc.revision(),
            })
        })
    }

    pub fn undo_lines(&self, handle: u64, redo: bool) -> Result<Option<RestoredLines>, String> {
        // 閉じられた文書の元に戻すは「何もない」であってエラーではない。
        if !self.state.docs.lock().unwrap().contains_key(&handle) {
            return Ok(None);
        }
        self.with_doc(handle, |doc| {
            Ok(
                (if redo { doc.redo() } else { doc.undo() })?.map(|restored| RestoredLines {
                    state: restored.state,
                    touched_from: restored.touched_from,
                    line_count: restored.line_count,
                    clean: doc.is_clean(),
                    revision: doc.revision(),
                }),
            )
        })
    }

    pub fn save_document(&self, handle: u64, path: String) -> Result<(), String> {
        self.with_doc(handle, |doc| doc.save(&path))
    }

    pub fn close_document(&self, handle: u64) {
        if let Some(generation) = self.state.searches.lock().unwrap().remove(&handle) {
            generation.fetch_add(1, Ordering::Relaxed);
        }
        self.state.docs.lock().unwrap().remove(&handle);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn prepare_search(
        &self,
        handle: u64,
        query: String,
        regex: bool,
        case_sensitive: bool,
        needle: char,
        from: usize,
        end: usize,
        after_col: Option<usize>,
    ) -> Result<SearchJob, String> {
        let generation = self
            .state
            .searches
            .lock()
            .unwrap()
            .get(&handle)
            .cloned()
            .ok_or_else(|| "文書はもう閉じられています".to_string())?;
        let ticket = generation.fetch_add(1, Ordering::Relaxed) + 1;
        let snapshot = self.with_doc(handle, |doc| doc.search_snapshot())?;
        let query = CompiledQuery::compile(&query, regex, case_sensitive, needle)?;
        Ok(SearchJob {
            application: self.clone(),
            handle,
            snapshot,
            query,
            from,
            end,
            after_col,
            generation,
            ticket,
        })
    }

    pub fn cancel_search(&self, handle: u64) {
        if let Some(generation) = self.state.searches.lock().unwrap().get(&handle) {
            generation.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn estimate_matches(
        &self,
        handle: u64,
        query: String,
        regex: bool,
        case_sensitive: bool,
    ) -> Result<usize, String> {
        let query = CompiledQuery::compile(&query, regex, case_sensitive, '\0')?;
        self.with_doc(handle, |doc| doc.estimate_matches(&query.pattern))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn replace_all(
        &self,
        handle: u64,
        base_revision: u64,
        group: u64,
        from_line: usize,
        to_line: usize,
        query: String,
        replacement: String,
        case_sensitive: bool,
        before: String,
        after: String,
    ) -> Result<EditApplied, String> {
        let pattern = regex::RegexBuilder::new(&regex::escape(&query))
            .case_insensitive(!case_sensitive)
            .build()
            .map_err(|e| format!("無効な検索語です: {e}"))?;
        let op = crate::operation_log::BulkOperation::ReplaceAll {
            from_line,
            to_line,
            query,
            replacement,
            case_sensitive,
            pattern: Arc::new(pattern),
        };
        self.with_doc(handle, |doc| {
            let line_count = doc.apply_bulk_operation(base_revision, group, op, &before, &after)?;
            Ok(EditApplied {
                line_count,
                revision: doc.revision(),
            })
        })
    }

    pub fn lines_containing(
        &self,
        handle: u64,
        from: usize,
        to: usize,
        needle: char,
    ) -> Result<Vec<usize>, String> {
        self.with_doc(handle, |doc| doc.lines_containing(from, to, needle))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn copy_range(
        &self,
        handle: u64,
        from: usize,
        first: Option<String>,
        to: usize,
        last: Option<String>,
        overrides: Vec<(usize, String)>,
    ) -> Result<String, String> {
        self.with_doc(handle, |doc| {
            doc.assemble(from, first, to, last, &overrides.into_iter().collect())
        })
    }

    pub fn handle_gui_event(&self, event: GuiEvent) -> GuiAction {
        match event {
            GuiEvent::CloseRequested => GuiAction::HideWindow,
            GuiEvent::TraySelected(TrayAction::Open) | GuiEvent::SecondInstance => {
                GuiAction::ShowWindow
            }
            GuiEvent::TraySelected(TrayAction::Quit) => GuiAction::Exit,
            GuiEvent::GlobalShortcut(_) => GuiAction::ToggleWindow,
        }
    }

    pub fn read_settings(&self, config_dir: Option<PathBuf>) -> String {
        settings_path(config_dir)
            .and_then(|path| std::fs::read_to_string(path).ok())
            .unwrap_or_default()
    }

    pub fn prepare_settings_write(
        &self,
        config_dir: Option<PathBuf>,
        contents: String,
    ) -> Result<SettingsWrite, String> {
        let Some(path) = settings_path(config_dir) else {
            return Err("設定の保存先がありません".to_string());
        };
        Ok(SettingsWrite { path, contents })
    }

    /// 文書の本体から下書きを書きます。最初の行はドキュメントのパス、続きが本文。
    pub fn save_draft(
        &self,
        config_dir: Option<PathBuf>,
        handle: u64,
        id: String,
        path: Option<String>,
    ) -> Result<(), String> {
        let Some(dir) = drafts_dir(config_dir) else {
            return Err("下書きの保存先がありません".to_string());
        };
        self.with_doc(handle, |doc| {
            let file = std::fs::File::create(dir.join(draft_name(&id)))
                .map_err(|e| format!("下書きを保存できませんでした: {e}"))?;
            let mut out = std::io::BufWriter::new(file);
            doc.write_draft(&mut out, path.as_deref())
        })
    }

    /// 保存済みファイル、未保存なら下書きファイルのサイズを返します。
    /// どちらも読めないときは `None` を返します（呼び出し側が不明として扱います）。
    pub fn file_size(
        &self,
        config_dir: Option<PathBuf>,
        path: Option<String>,
        id: String,
    ) -> Option<u64> {
        let target = match path {
            Some(p) => PathBuf::from(p),
            None => drafts_dir(config_dir)?.join(draft_name(&id)),
        };
        std::fs::metadata(&target).map(|m| m.len()).ok()
    }

    pub fn remove_draft(&self, config_dir: Option<PathBuf>, id: String) {
        if let Some(dir) = drafts_dir(config_dir) {
            std::fs::remove_file(dir.join(draft_name(&id))).ok();
        }
    }

    pub fn read_drafts(&self, config_dir: Option<PathBuf>) -> Vec<Draft> {
        let Some(dir) = drafts_dir(config_dir) else {
            return Vec::new();
        };
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut drafts: Vec<Draft> = entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                let id = path.file_stem()?.to_string_lossy().into_owned();
                let file = std::fs::read_to_string(&path).ok()?;
                let mut lines = file.lines();
                let first = lines.next().unwrap_or_default();
                if first == "// PLANETEXT_DRAFT_REF_V1" {
                    let orig_path = lines.next().unwrap_or_default().to_string();
                    let status = lines.next().unwrap_or_default();
                    if status == "CLEAN" {
                        // 未変更の下書き: 元ファイルが存在すればその内容を読み出す
                        let contents = std::fs::read_to_string(&orig_path).unwrap_or_default();
                        Some(Draft {
                            id,
                            path: Some(orig_path),
                            contents,
                        })
                    } else {
                        // 変更がある場合は後続のテキスト
                        let remaining: Vec<&str> = lines.collect();
                        Some(Draft {
                            id,
                            path: Some(orig_path),
                            contents: remaining.join("\n"),
                        })
                    }
                } else {
                    let first = first.trim_end_matches(['\r', '\n']);
                    let contents = file.split_once('\n').map_or("", |(_, rest)| rest);
                    Some(Draft {
                        id,
                        path: (!first.is_empty()).then(|| first.to_string()),
                        contents: contents.to_string(),
                    })
                }
            })
            .collect();
        // タブは開かれた順序で戻ります。
        drafts.sort_by(|a, b| a.id.cmp(&b.id));
        drafts
    }

    pub fn clear_drafts(&self, config_dir: Option<PathBuf>) {
        if let Some(dir) = drafts_dir(config_dir) {
            std::fs::remove_dir_all(dir).ok();
        }
    }

    pub fn set_dirty(&self, dirty: bool) {
        *self.state.dirty.lock().unwrap() = dirty;
    }

    pub fn clear_dirty(&self) {
        if let Ok(mut dirty) = self.state.dirty.lock() {
            *dirty = false;
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.state.dirty.lock().map(|dirty| *dirty).unwrap_or(false)
    }
}

/// 設定ファイルが存在する場所: アプリ独自の構成ディレクトリ内の `settings.toml`。
fn settings_path(config_dir: Option<PathBuf>) -> Option<PathBuf> {
    let dir = config_dir?;
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("settings.toml"))
}

/// 下書きが存在する場所: 開いているドキュメントごとに、設定の横に 1 つのファイル。
///
/// 下書きは、保存されているかどうかに関係なく、現在画面上に表示されているものです。ドキュメント自体のファイルは、ユーザーが保存するときにのみ書き込まれるため、ユーザーがファイル自体に触れることがない限り、クラッシュや停電によるコストは発生しません。
fn drafts_dir(config_dir: Option<PathBuf>) -> Option<PathBuf> {
    let dir = config_dir?.join("drafts");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// 下書きの名前は数字に保たれるため、ID にアクセスすることはできません。
fn draft_name(id: &str) -> String {
    let digits: String = id.chars().filter(char::is_ascii_digit).collect();
    format!(
        "{}.draft",
        if digits.is_empty() {
            "0"
        } else {
            digits.as_str()
        }
    )
}

#[cfg(test)]
mod tests {
    use super::{global_shortcut_settings, Application, GuiAction, GuiEvent, TrayAction};

    #[test]
    fn global_shortcut_settings_keep_defaults() {
        let settings = global_shortcut_settings("");
        assert!(settings.enabled);
        assert_eq!(settings.key, "Ctrl+Shift+M");
    }

    #[test]
    fn global_shortcut_settings_read_overrides() {
        let settings = global_shortcut_settings(
            "global_shortcut = false\nglobal_shortcut_key = 'Ctrl+Alt+P'\n",
        );
        assert!(!settings.enabled);
        assert_eq!(settings.key, "Ctrl+Alt+P");
    }

    #[test]
    fn tray_quit_exits_without_confirm_dialog() {
        let application = Application::default();
        application.set_dirty(true);
        assert!(matches!(
            application.handle_gui_event(GuiEvent::TraySelected(TrayAction::Quit)),
            GuiAction::Exit
        ));
        application.set_dirty(false);
        assert!(matches!(
            application.handle_gui_event(GuiEvent::TraySelected(TrayAction::Quit)),
            GuiAction::Exit
        ));
    }

    #[test]
    fn framework_events_produce_framework_neutral_actions() {
        let application = Application::default();
        assert!(matches!(
            application.handle_gui_event(GuiEvent::CloseRequested),
            GuiAction::HideWindow
        ));
        assert!(matches!(
            application.handle_gui_event(GuiEvent::SecondInstance),
            GuiAction::ShowWindow
        ));
        assert!(matches!(
            application.handle_gui_event(GuiEvent::GlobalShortcut("toggle".into())),
            GuiAction::ToggleWindow
        ));
    }

    #[test]
    fn create_document_from_draft_returns_an_already_populated_handle() {
        let application = Application::default();
        let doc = application
            .create_document_from_draft(vec!["draft one".into(), "draft two".into()])
            .unwrap();

        assert_eq!(doc.line_count, 2);
        assert_eq!(
            application.read_lines(doc.handle, 0, usize::MAX).unwrap(),
            vec!["draft one", "draft two"]
        );
        assert!(!application
            .with_doc(doc.handle, |doc| Ok(doc.is_clean()))
            .unwrap());
        assert!(application.undo_lines(doc.handle, false).unwrap().is_none());
    }

    #[test]
    fn create_document_still_returns_a_clean_empty_document() {
        let application = Application::default();
        let doc = application.create_document();

        assert_eq!(doc.line_count, 1);
        assert_eq!(
            application.read_lines(doc.handle, 0, usize::MAX).unwrap(),
            vec![""]
        );
        assert!(application
            .with_doc(doc.handle, |doc| Ok(doc.is_clean()))
            .unwrap());
        assert!(application.undo_lines(doc.handle, false).unwrap().is_none());
    }

    #[test]
    fn save_and_read_untitled_draft() {
        let application = Application::default();
        let doc = application.create_document();
        let temp_dir = std::env::temp_dir().join(format!("planetext_test_draft_{}", doc.handle));
        std::fs::create_dir_all(&temp_dir).unwrap();

        application
            .save_draft(Some(temp_dir.clone()), doc.handle, "1".into(), None)
            .unwrap();

        let drafts = application.read_drafts(Some(temp_dir.clone()));
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].id, "1");
        assert_eq!(drafts[0].path, None);

        application.clear_drafts(Some(temp_dir.clone()));
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    /// 【回帰防止テスト】
    /// 実ファイルを持つ文書の下書き保存時、全文ダンプではなく元ファイル参照が記録され、
    /// read_drafts で元ファイルと関連付けられた下書きとして復元されることを保証する。
    #[test]
    fn save_and_read_referenced_file_draft() {
        let application = Application::default();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let file_path =
            std::env::temp_dir().join(format!("planetext_draft_target_{timestamp}.txt"));
        std::fs::write(&file_path, "original file line 1\noriginal file line 2").unwrap();

        let doc = application
            .open_document(file_path.to_str().unwrap().to_string())
            .unwrap();
        let temp_dir = std::env::temp_dir().join(format!(
            "planetext_test_ref_draft_{timestamp}_{}",
            doc.handle
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();

        application
            .save_draft(
                Some(temp_dir.clone()),
                doc.handle,
                "1".into(),
                Some(file_path.to_str().unwrap().into()),
            )
            .unwrap();

        let drafts = application.read_drafts(Some(temp_dir.clone()));
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].id, "1");
        assert_eq!(drafts[0].path.as_deref(), Some(file_path.to_str().unwrap()));
        assert!(drafts[0].contents.contains("original file line 1"));

        application.clear_drafts(Some(temp_dir.clone()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        let _ = std::fs::remove_file(&file_path);
    }
}
