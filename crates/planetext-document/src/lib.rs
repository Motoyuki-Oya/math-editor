//! 文書エンジン。GUIフレームワークを知らない。

mod document;
mod edit_buffers;
mod operation_log;
mod persistence;
mod piece_tree;
mod search;
mod search_index;
mod source;
mod transaction;
#[cfg(test)]
mod test_utils;

pub use transaction::FileTransaction;

use document::Document;
pub use search::{CompiledQuery, ScanHit};
use search::SearchSpec;
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
    pub line_count: Option<usize>,
    pub bytes: usize,
    pub encoding: String,
    pub line_ending: String,
    pub revision: u64,
    pub clean: bool,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ReopenedDocument {
    pub line_count: Option<usize>,
    pub encoding: String,
    pub line_ending: String,
    pub revision: u64,
}

pub use document::SpliceEdit;

/// 元に戻す・やり直すの結果。`state` は frontend が預けた控えそのもの。
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct RestoredLines {
    pub state: String,
    pub touched_from: usize,
    pub line_count: usize,
    pub clean: bool,
    pub revision: u64,
    pub modified_lines: Vec<usize>,
    #[serde(default)]
    pub splices: Vec<SpliceEdit>,
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

#[derive(serde::Serialize, Debug, Clone)]
pub struct SearchPage {
    pub hits: Vec<ScanHit>,
    pub scanned_to: usize,
    pub cancelled: bool,
    pub total_matches: Option<usize>,
    pub current_index: Option<usize>,
}

/// 下書きファイルは最初の行にドキュメントのパスがあり、その後にドキュメント自体が含まれているため、復元されたドラフトではそれがどのファイルに属しているかがわかります。
#[derive(serde::Serialize)]
pub struct Draft {
    pub id: String,
    pub path: Option<String>,
    pub contents: String,
    pub clean: bool,
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
    forward: bool,
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
                forward: self.forward,
            },
            &|| self.generation.load(Ordering::Relaxed) != self.ticket,
        )?;
        let snapshot_cache = self.snapshot.search_cache.take();
        let (mapped_hits, total_matches, current_index) = self
            .application
            .with_doc(self.handle, |doc| {
                if let Some(cache) = snapshot_cache {
                    doc.search_cache = Some(cache);
                }
                let mapped = doc.map_search_hits(&self.snapshot, found.hits);
                Ok((mapped, found.total_matches, found.current_index))
            })
            .unwrap_or_default();

        Ok(SearchPage {
            hits: mapped_hits,
            scanned_to: found.scanned_to,
            cancelled: found.cancelled,
            total_matches,
            current_index,
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
        let parent = self
            .path
            .parent()
            .ok_or_else(|| "親ディレクトリがありません".to_string())?;
        let mut tx = FileTransaction::begin(parent)?;
        tx.add_file_bytes(&self.path, self.contents.as_bytes())?;
        tx.commit()
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
        // 走査未完了の間は行数は未確定（None）として返す。確定済みなら Some(count)。
        let line_count = doc.confirmed_line_count();
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
            clean: doc.is_clean(),
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
                let line_count = if scan.is_some() {
                    None
                } else {
                    Some(doc.line_count())
                };
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

    /// 下書きIDから文書を復元する。元ファイルがある場合は全文ダンプではなく
    /// 元ファイルを開いて未保存差分を適用するため、巨大ファイルでも一瞬で安全に復旧できる。
    pub fn open_draft(
        &self,
        config_dir: Option<PathBuf>,
        id: String,
    ) -> Result<OpenedDocument, String> {
        let Some(dir) = drafts_dir(config_dir) else {
            return Err("下書きディレクトリがありません".to_string());
        };
        let path = dir.join(draft_name(&id));
        let file =
            std::fs::read_to_string(&path).map_err(|e| format!("下書きを開けませんでした: {e}"))?;
        let mut lines = file.lines();
        let first = lines.next().unwrap_or_default();
        if first == "// PLANETEXT_DRAFT_REF_V2" {
            let orig_path = lines.next().unwrap_or_default();
            let saved_line_count: usize = lines.next().unwrap_or_default().parse().unwrap_or(0);
            let status = lines.next().unwrap_or_default();
            let mut diffs = Vec::new();
            let mut head_offset = 0;
            let mut has_explicit_head = false;
            if status == "DIFF" {
                if let Some(header_str) = lines.next() {
                    let parts: Vec<&str> = header_str.split_whitespace().collect();
                    let count: usize = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
                    if parts.len() >= 2 {
                        head_offset = parts[1].parse().unwrap_or(count);
                        has_explicit_head = true;
                    } else {
                        head_offset = count;
                    }
                    diffs.reserve(count);
                    for _ in 0..count {
                        let diff = crate::persistence::DraftDiff::read_from(&mut lines)
                            .ok_or_else(|| {
                                "下書きファイルが破損しています（差分が途切れています）".to_string()
                            })?;
                        diffs.push(diff);
                    }
                }
            }

            let (mut doc, scan) = Document::open(orig_path)?;
            let bg_index = doc.take_background_index();

            // active 差分だけでなく、保留中の Redo 差分の位置も含める。
            let max_needed_line = diffs
                .iter()
                .map(|d| d.from_line + d.removed_lines)
                .max()
                .unwrap_or(0);

            // 過去の重大ミスと再発防止の記録:
            // 下書きに保存された行数が 0（未確定）の場合、走査を打ち切って確定させてはならない。
            // また、万一過去のバグで先頭チャンクの暫定行数（doc.count）がそのまま保存されていた汚染下書きであっても、
            // それを確定値と誤認して背景走査を打ち切らないよう防護する。
            let is_stale_provisional_count =
                doc.is_scanning() && saved_line_count == doc.line_count();
            if saved_line_count > 0 && !is_stale_provisional_count {
                doc.confirm_scan_with_total_lines(saved_line_count);
            } else if max_needed_line >= doc.line_count() {
                doc.confirm_scan_with_total_lines(max_needed_line);
            }

            let active_count = if has_explicit_head {
                head_offset.min(diffs.len())
            } else {
                diffs.len()
            };
            let mut active_diffs = Vec::with_capacity(active_count);
            let mut redo_diffs = Vec::with_capacity(diffs.len().saturating_sub(active_count));
            for (i, diff) in diffs.into_iter().enumerate() {
                if i < active_count {
                    active_diffs.push(diff);
                } else {
                    redo_diffs.push(diff);
                }
            }
            doc.set_pending_redo_diffs(redo_diffs);

            if let Some(scan) = scan {
                // 下書き復元は 0 秒即時リターンを保証するため、走査は常にバックグラウンドで回す。
                std::thread::spawn(move || {
                    let _ = scan.run();
                });
            }

            if !active_diffs.is_empty() {
                doc.apply_draft_diffs(active_diffs)
                    .map_err(|e| format!("下書きの操作ログを再生できませんでした: {e}"))?;
            }

            let opened = self.adopt(doc);
            if let Some(bg_index) = bg_index {
                std::thread::spawn(move || {
                    bg_index.run();
                });
            }
            Ok(opened)
        } else if first == "// PLANETEXT_DRAFT_REF_V1" {
            let orig_path = lines.next().unwrap_or_default();
            let status = lines.next().unwrap_or_default();
            let mut diffs = Vec::new();
            if status == "DIFF" {
                if let Some(count_str) = lines.next() {
                    let count: usize = count_str.parse().unwrap_or(0);
                    diffs.reserve(count);
                    for _ in 0..count {
                        if let Some(diff) = crate::persistence::DraftDiff::read_from_v1(&mut lines)
                        {
                            diffs.push(diff);
                        }
                    }
                }
            }

            let (mut doc, scan) = Document::open(orig_path)?;
            let bg_index = doc.take_background_index();

            let max_needed_line = diffs
                .iter()
                .map(|d| d.from_line + d.removed_lines)
                .max()
                .unwrap_or(0);
            if max_needed_line >= doc.line_count() {
                doc.confirm_scan_with_total_lines(max_needed_line);
            }

            if let Some(scan) = scan {
                std::thread::spawn(move || {
                    let _ = scan.run();
                });
            }

            if !diffs.is_empty() {
                doc.apply_draft_diffs(diffs)
                    .map_err(|e| format!("下書きの操作ログを再生できませんでした: {e}"))?;
            }

            let opened = self.adopt(doc);
            if let Some(bg_index) = bg_index {
                std::thread::spawn(move || {
                    bg_index.run();
                });
            }
            Ok(opened)
        } else {
            let contents = file.split_once('\n').map_or("", |(_, rest)| rest);
            let lines: Vec<String> = contents.lines().map(String::from).collect();
            let doc = Document::from_draft(lines);
            Ok(self.adopt(doc))
        }
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
        self.cancel_search(handle);
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
        self.cancel_search(handle);
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
        self.cancel_search(handle);
        self.with_doc(handle, |doc| {
            Ok(
                (if redo { doc.redo() } else { doc.undo() })?.map(|restored| RestoredLines {
                    state: restored.state,
                    touched_from: restored.touched_from,
                    line_count: restored.line_count,
                    clean: doc.is_clean(),
                    revision: doc.revision(),
                    modified_lines: doc.modified_lines(),
                    splices: restored.splices,
                }),
            )
        })
    }

    pub fn save_document(&self, handle: u64, path: String) -> Result<(), String> {
        self.cancel_search(handle);
        self.with_doc(handle, |doc| doc.save(&path))
    }

    pub fn close_document(&self, handle: u64) {
        self.cancel_search(handle);
        self.state.searches.lock().unwrap().remove(&handle);
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
        forward: bool,
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
            forward,
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
        self.with_doc(handle, |doc| doc.estimate_matches(query.matcher.as_ref()))
    }

    pub fn search_index_progress(&self, handle: u64) -> Result<Option<(usize, usize)>, String> {
        self.with_doc(handle, |doc| Ok(doc.search_index_progress()))
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
        if let Some(ref dir) = config_dir {
            let _ = FileTransaction::recover(dir);
        }
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
    /// 一時ファイル＋ジャーナル＋原子的置換によりクラッシュ時にも安全性を保証する。
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
            let target = dir.join(draft_name(&id));
            let mut tx = FileTransaction::begin(&dir)?;
            tx.add_file(&target, |out| {
                doc.write_draft(out, path.as_deref())
            })?;
            tx.commit()
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
        let _ = FileTransaction::recover(&dir);
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
                if first == "// PLANETEXT_DRAFT_REF_V2" || first == "// PLANETEXT_DRAFT_REF_V1" {
                    let orig_path = lines.next().unwrap_or_default().to_string();
                    let status = if first == "// PLANETEXT_DRAFT_REF_V2" {
                        let _line_count = lines.next();
                        lines.next().unwrap_or_default()
                    } else {
                        lines.next().unwrap_or_default()
                    };
                    let mut clean = status == "CLEAN";
                    if status == "DIFF" {
                        if let Some(header_str) = lines.next() {
                            let parts: Vec<&str> = header_str.split_whitespace().collect();
                            let head_offset: usize =
                                parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(1);
                            if head_offset == 0 {
                                clean = true;
                            }
                        }
                    }
                    Some(Draft {
                        id,
                        path: Some(orig_path),
                        contents: String::new(),
                        clean,
                    })
                } else {
                    let first = first.trim_end_matches(['\r', '\n']);
                    let contents = file.split_once('\n').map_or("", |(_, rest)| rest);
                    Some(Draft {
                        id,
                        path: (!first.is_empty()).then(|| first.to_string()),
                        contents: contents.to_string(),
                        clean: false,
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

        assert_eq!(doc.line_count, Some(2));
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

        assert_eq!(doc.line_count, Some(1));
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

        // 実ファイル下書きの本文復元は open_draft で行われる
        let reopened = application
            .open_draft(Some(temp_dir.clone()), "1".into())
            .unwrap();
        let read = application.read_lines(reopened.handle, 0, 2).unwrap();
        assert_eq!(read.lines[0], "original file line 1");

        application.clear_drafts(Some(temp_dir.clone()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        let _ = std::fs::remove_file(&file_path);
    }

    /// 【回帰防止テスト】
    /// 編集された実ファイル文書の下書き保存時、全文ダンプではなく「元ファイル参照＋差分JSON」が
    /// 記録され、read_drafts または open_draft で編集後の状態が正確に復元されることを保証する。
    #[test]
    fn save_and_read_modified_referenced_file_draft() {
        let application = Application::default();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let file_path =
            std::env::temp_dir().join(format!("planetext_draft_modified_{timestamp}.txt"));
        std::fs::write(&file_path, "base line 1\nbase line 2\nbase line 3").unwrap();

        let doc = application
            .open_document(file_path.to_str().unwrap().to_string())
            .unwrap();
        // 2行目を編集
        application
            .replace_lines(
                doc.handle,
                1,
                2,
                vec!["MODIFIED LINE 2".into()],
                1,
                "".into(),
                "".into(),
            )
            .unwrap();

        let temp_dir = std::env::temp_dir().join(format!(
            "planetext_test_mod_draft_{timestamp}_{}",
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

        // 1. 下書きファイルの中身を直接検査: 全文ダンプではなく DIFF フォーマットであること
        let draft_file_path = temp_dir.join("drafts").join("1.draft");
        let draft_content = std::fs::read_to_string(&draft_file_path).unwrap();
        assert!(
            draft_content.contains("DIFF"),
            "下書きファイルに DIFF マーカーが含まれること"
        );
        assert!(
            draft_content.contains("// PLANETEXT_DRAFT_REF_V2")
                || draft_content.contains("// PLANETEXT_DRAFT_REF_V1"),
            "下書きファイルに参照マーカーが含まれること"
        );

        // 2. read_drafts で元ファイルパスが正しく返されること（全文生成は行わず最速）
        let drafts = application.read_drafts(Some(temp_dir.clone()));
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].path.as_deref(), Some(file_path.to_str().unwrap()));

        // 3. open_draft API で直接 OpenedDocument として一発で復元できること
        let reopened = application
            .open_draft(Some(temp_dir.clone()), "1".into())
            .unwrap();
        let read = application.read_lines(reopened.handle, 0, 3).unwrap();
        assert_eq!(
            read.lines,
            vec!["base line 1", "MODIFIED LINE 2", "base line 3"]
        );

        application.clear_drafts(Some(temp_dir.clone()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        let _ = std::fs::remove_file(&file_path);
    }

    #[test]
    fn test_draft_replay_consecutive_edits() {
        let application = Application::default();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let file_path = std::env::temp_dir().join(format!("planetext_consec_{timestamp}.txt"));
        std::fs::write(&file_path, "line 0\nline 1\nline 2\nline 3\nline 4").unwrap();

        let doc = application
            .open_document(file_path.to_str().unwrap().to_string())
            .unwrap();
        // 2行目を3回連続編集（ユーザーが文字をタイピングした状況の再現）
        application
            .replace_lines(
                doc.handle,
                2,
                3,
                vec!["step 1".into()],
                1,
                "".into(),
                "".into(),
            )
            .unwrap();
        application
            .replace_lines(
                doc.handle,
                2,
                3,
                vec!["step 2".into()],
                2,
                "".into(),
                "".into(),
            )
            .unwrap();
        application
            .replace_lines(
                doc.handle,
                2,
                3,
                vec!["step 3".into()],
                3,
                "".into(),
                "".into(),
            )
            .unwrap();

        let temp_dir = std::env::temp_dir().join(format!("planetext_consec_draft_{timestamp}"));
        std::fs::create_dir_all(&temp_dir).unwrap();

        application
            .save_draft(
                Some(temp_dir.clone()),
                doc.handle,
                "10".into(),
                Some(file_path.to_str().unwrap().into()),
            )
            .unwrap();

        // open_draft で元ファイルを読み込み、操作ログを再生してピースツリーを再構成
        let reopened = application
            .open_draft(Some(temp_dir.clone()), "10".into())
            .unwrap();
        let read = application.read_lines(reopened.handle, 0, 5).unwrap();
        assert_eq!(
            read.lines,
            vec!["line 0", "line 1", "step 3", "line 3", "line 4"]
        );

        application.clear_drafts(Some(temp_dir.clone()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        let _ = std::fs::remove_file(&file_path);
    }

    #[test]
    fn test_draft_replay_insert_delete() {
        let application = Application::default();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let file_path = std::env::temp_dir().join(format!("planetext_insdel_{timestamp}.txt"));
        std::fs::write(&file_path, "line 0\nline 1\nline 2\nline 3\nline 4\nline 5").unwrap();

        let doc = application
            .open_document(file_path.to_str().unwrap().to_string())
            .unwrap();
        // 1行目を削除
        application
            .replace_lines(doc.handle, 1, 2, vec![], 1, "".into(), "".into())
            .unwrap();
        // （削除後の座標系で）3行目の後ろに2行挿入
        application
            .replace_lines(
                doc.handle,
                3,
                3,
                vec!["NEW A".into(), "NEW B".into()],
                2,
                "".into(),
                "".into(),
            )
            .unwrap();

        let temp_dir = std::env::temp_dir().join(format!("planetext_insdel_draft_{timestamp}"));
        std::fs::create_dir_all(&temp_dir).unwrap();

        application
            .save_draft(
                Some(temp_dir.clone()),
                doc.handle,
                "11".into(),
                Some(file_path.to_str().unwrap().into()),
            )
            .unwrap();

        let reopened = application
            .open_draft(Some(temp_dir.clone()), "11".into())
            .unwrap();
        let read = application.read_lines(reopened.handle, 0, 7).unwrap();
        assert_eq!(
            read.lines,
            vec!["line 0", "line 2", "line 3", "NEW A", "NEW B", "line 4", "line 5"]
        );

        application.clear_drafts(Some(temp_dir.clone()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        let _ = std::fs::remove_file(&file_path);
    }

    #[test]
    fn test_draft_replay_stepwise_undo_modified_lines() {
        let application = Application::default();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let file_path = std::env::temp_dir().join(format!("planetext_stepundo_{timestamp}.txt"));
        std::fs::write(
            &file_path,
            "line 0\nline 1\nline 2\nline 3\nline 4\nline 5\n",
        )
        .unwrap();

        let doc = application
            .open_document(file_path.to_str().unwrap().to_string())
            .unwrap();

        // 1. 1行目編集
        application
            .replace_lines(
                doc.handle,
                1,
                2,
                vec!["line 1 EDITED".into()],
                1,
                "".into(),
                "".into(),
            )
            .unwrap();

        // 2. 5行目編集
        application
            .replace_lines(
                doc.handle,
                5,
                6,
                vec!["line 5 EDITED".into()],
                2,
                "".into(),
                "".into(),
            )
            .unwrap();

        let temp_dir = std::env::temp_dir().join(format!("planetext_stepundo_draft_{timestamp}"));
        std::fs::create_dir_all(&temp_dir).unwrap();

        application
            .save_draft(
                Some(temp_dir.clone()),
                doc.handle,
                "42".into(),
                Some(file_path.to_str().unwrap().into()),
            )
            .unwrap();

        // 3. 下書きから復元
        let reopened = application
            .open_draft(Some(temp_dir.clone()), "42".into())
            .unwrap();

        // 復元直後の確認: 1行目と5行目が編集済み
        let lines_after_restore = application.read_lines(reopened.handle, 0, 7).unwrap();
        assert_eq!(lines_after_restore.lines[1], "line 1 EDITED");
        assert_eq!(lines_after_restore.lines[5], "line 5 EDITED");

        // 4. 1回目の Undo（5行目の編集を取り消し）
        let undo1 = application
            .undo_lines(reopened.handle, false)
            .unwrap()
            .expect("undo 1");
        assert_eq!(
            undo1.modified_lines,
            vec![1],
            "5行目のUndo後は1行目のみハイライト"
        );
        assert!(!undo1.clean, "まだ1行目の変更があるので Dirty");

        let lines_after_undo1 = application.read_lines(reopened.handle, 0, 7).unwrap();
        assert_eq!(lines_after_undo1.lines[1], "line 1 EDITED");
        assert_eq!(lines_after_undo1.lines[5], "line 5");

        // 5. 2回目の Undo（1行目の編集を取り消し）
        let undo2 = application
            .undo_lines(reopened.handle, false)
            .unwrap()
            .expect("undo 2");
        assert_eq!(
            undo2.modified_lines,
            Vec::<usize>::new(),
            "1行目もUndo後はハイライトなし"
        );
        assert!(undo2.clean, "元ファイルと完全同一になったので Clean");

        let lines_after_undo2 = application.read_lines(reopened.handle, 0, 7).unwrap();
        assert_eq!(lines_after_undo2.lines[1], "line 1");
        assert_eq!(lines_after_undo2.lines[5], "line 5");

        application.clear_drafts(Some(temp_dir.clone()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        let _ = std::fs::remove_file(&file_path);
    }

    #[test]
    fn benchmark_draft_100mb() {
        use std::io::{BufWriter, Write};
        let application = Application::default();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let file_path = std::env::temp_dir().join(format!("planetext_bench100m_{timestamp}.txt"));

        // 1. 100MB（約200万行）のファイルを高速生成
        let t0 = std::time::Instant::now();
        {
            let file = std::fs::File::create(&file_path).unwrap();
            let mut writer = BufWriter::with_capacity(1024 * 1024, file);
            let line_data = b"This is a benchmark line for testing 100MB large file draft save and restore performance.\n";
            let line_count = 100 * 1024 * 1024 / line_data.len();
            for i in 0..line_count {
                if i % 100_000 == 0 {
                    let custom = format!("Header milestone at line {i}\n");
                    writer.write_all(custom.as_bytes()).unwrap();
                } else {
                    writer.write_all(line_data).unwrap();
                }
            }
            writer.flush().unwrap();
        }
        let gen_time = t0.elapsed();
        let file_size = std::fs::metadata(&file_path).unwrap().len();
        println!("\n[BENCH 100MB] File generated: size={file_size} bytes in {gen_time:?}");

        // 2. ファイルを開く
        let t1 = std::time::Instant::now();
        let doc = application
            .open_document(file_path.to_str().unwrap().to_string())
            .unwrap();
        // 走査完了を待機
        let job = application.finish_document(doc.handle).unwrap();
        let total_lines = loop {
            if let Some(count) = job.poll().unwrap() {
                break count;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        let open_and_scan_time = t1.elapsed();
        println!(
            "[BENCH 100MB] Open and scan finished: {total_lines} lines in {open_and_scan_time:?}"
        );

        // 3. 先頭（100行目）のみを編集した場合の測定（日常編集シナリオ）
        application
            .replace_lines(
                doc.handle,
                100,
                101,
                vec!["EDITED LINE AT HEAD 100".into()],
                1,
                "".into(),
                "".into(),
            )
            .unwrap();

        let temp_dir = std::env::temp_dir().join(format!("planetext_bench100m_draft_{timestamp}"));
        std::fs::create_dir_all(&temp_dir).unwrap();

        // 先頭のみ編集での下書き保存＆即時復元
        application
            .save_draft(
                Some(temp_dir.clone()),
                doc.handle,
                "head100".into(),
                Some(file_path.to_str().unwrap().into()),
            )
            .unwrap();

        let t_head_restore = std::time::Instant::now();
        let reopened_head = application
            .open_draft(Some(temp_dir.clone()), "head100".into())
            .unwrap();
        let head_restore_time = t_head_restore.elapsed();
        println!(
            "[BENCH 100MB] Open draft (Head-only edit, non-blocking scan): {head_restore_time:?}"
        );
        let head_read = application
            .read_lines(reopened_head.handle, 100, 1)
            .unwrap();
        assert_eq!(head_read.lines, vec!["EDITED LINE AT HEAD 100"]);

        // 4. さらに中間（100万行目）、末尾付近（total_lines - 10 行目）を編集（全領域編集シナリオ）
        let mid_line = total_lines / 2;
        let end_line = total_lines.saturating_sub(10);

        application
            .replace_lines(
                doc.handle,
                mid_line,
                mid_line + 1,
                vec!["EDITED LINE AT MID".into()],
                2,
                "".into(),
                "".into(),
            )
            .unwrap();
        application
            .replace_lines(
                doc.handle,
                end_line,
                end_line + 1,
                vec!["EDITED LINE AT END".into()],
                3,
                "".into(),
                "".into(),
            )
            .unwrap();

        // 5. 全領域編集での下書き保存の測定
        let t2 = std::time::Instant::now();
        application
            .save_draft(
                Some(temp_dir.clone()),
                doc.handle,
                "bench100".into(),
                Some(file_path.to_str().unwrap().into()),
            )
            .unwrap();
        let draft_save_time = t2.elapsed();
        let draft_path = temp_dir.join("drafts").join("100.draft");
        let draft_file_size = std::fs::metadata(&draft_path).unwrap().len();
        println!(
            "[BENCH 100MB] Save draft (3-locations): {draft_save_time:?} (draft file size: {draft_file_size} bytes)"
        );

        // 6. 全領域編集での下書き復元の測定（中間・末尾を含むため同期走査で完備）
        let t3 = std::time::Instant::now();
        let reopened = application
            .open_draft(Some(temp_dir.clone()), "bench100".into())
            .unwrap();
        let draft_restore_time = t3.elapsed();
        println!(
            "[BENCH 100MB] Open draft (Full-range edits with sync scan): {draft_restore_time:?}"
        );

        // 7. 編集された3箇所の行が正しく復元されているか検証
        let head_read = application.read_lines(reopened.handle, 100, 1).unwrap();
        assert_eq!(head_read.lines, vec!["EDITED LINE AT HEAD 100"]);

        let mid_read = application
            .read_lines(reopened.handle, mid_line, 1)
            .unwrap();
        assert_eq!(mid_read.lines, vec!["EDITED LINE AT MID"]);

        let end_read = application
            .read_lines(reopened.handle, end_line, 1)
            .unwrap();
        assert_eq!(end_read.lines, vec!["EDITED LINE AT END"]);

        println!("[BENCH 100MB] Verification SUCCESS: Head, Mid, End edits restored accurately!");

        application.clear_drafts(Some(temp_dir.clone()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        let _ = std::fs::remove_file(&file_path);
    }

    #[test]
    #[ignore] // 800MBの大規模ベンチマークのため、明示的な実行時のみ動作
    fn benchmark_draft_800mb() {
        use std::io::{BufWriter, Write};
        let application = Application::default();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let file_path = std::env::temp_dir().join(format!("planetext_bench800m_{timestamp}.txt"));

        // 1. 800MB（約930万行）のファイルを高速生成
        let t0 = std::time::Instant::now();
        {
            let file = std::fs::File::create(&file_path).unwrap();
            let mut writer = BufWriter::with_capacity(4 * 1024 * 1024, file);
            let line_data = b"This is a benchmark line for testing 800MB ultra large file draft save and restore performance.\n";
            let target_bytes: usize = 800 * 1024 * 1024;
            let line_count = target_bytes / line_data.len();
            for i in 0..line_count {
                if i % 500_000 == 0 {
                    let custom = format!("Header milestone at line {i}\n");
                    writer.write_all(custom.as_bytes()).unwrap();
                } else {
                    writer.write_all(line_data).unwrap();
                }
            }
            writer.flush().unwrap();
        }
        let gen_time = t0.elapsed();
        let file_size = std::fs::metadata(&file_path).unwrap().len();
        println!("\n[BENCH 800MB] File generated: size={file_size} bytes in {gen_time:?}");

        // 2. ファイルを開く
        let t1 = std::time::Instant::now();
        let doc = application
            .open_document(file_path.to_str().unwrap().to_string())
            .unwrap();
        // 走査完了を待機
        let job = application.finish_document(doc.handle).unwrap();
        let total_lines = loop {
            if let Some(count) = job.poll().unwrap() {
                break count;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        let open_and_scan_time = t1.elapsed();
        println!(
            "[BENCH 800MB] Open and scan finished: {total_lines} lines in {open_and_scan_time:?}"
        );

        // 3. 先頭（100行目）のみを編集した場合の測定（日常編集シナリオ）
        application
            .replace_lines(
                doc.handle,
                100,
                101,
                vec!["EDITED LINE AT HEAD 100 IN 800MB".into()],
                1,
                "".into(),
                "".into(),
            )
            .unwrap();

        let temp_dir = std::env::temp_dir().join(format!("planetext_bench800m_draft_{timestamp}"));
        std::fs::create_dir_all(&temp_dir).unwrap();

        // 先頭のみ編集での下書き保存＆即時復元
        let t_save_head = std::time::Instant::now();
        application
            .save_draft(
                Some(temp_dir.clone()),
                doc.handle,
                "head800".into(),
                Some(file_path.to_str().unwrap().into()),
            )
            .unwrap();
        let head_save_time = t_save_head.elapsed();
        println!("[BENCH 800MB] Save draft (Head-only): {head_save_time:?}");

        let t_head_restore = std::time::Instant::now();
        let reopened_head = application
            .open_draft(Some(temp_dir.clone()), "head800".into())
            .unwrap();
        let head_restore_time = t_head_restore.elapsed();
        println!(
            "[BENCH 800MB] Open draft (Head-only edit, non-blocking scan): {head_restore_time:?}"
        );
        let head_read = application
            .read_lines(reopened_head.handle, 100, 1)
            .unwrap();
        assert_eq!(head_read.lines, vec!["EDITED LINE AT HEAD 100 IN 800MB"]);

        // 4. 中間（450万行目）、末尾付近（total_lines - 10 行目）を編集（全領域編集シナリオ）
        let mid_line = total_lines / 2;
        let end_line = total_lines.saturating_sub(10);

        application
            .replace_lines(
                doc.handle,
                mid_line,
                mid_line + 1,
                vec!["EDITED LINE AT MID IN 800MB".into()],
                2,
                "".into(),
                "".into(),
            )
            .unwrap();
        application
            .replace_lines(
                doc.handle,
                end_line,
                end_line + 1,
                vec!["EDITED LINE AT END IN 800MB".into()],
                3,
                "".into(),
                "".into(),
            )
            .unwrap();

        // 5. 全領域編集での下書き保存の測定
        let t2 = std::time::Instant::now();
        application
            .save_draft(
                Some(temp_dir.clone()),
                doc.handle,
                "bench800".into(),
                Some(file_path.to_str().unwrap().into()),
            )
            .unwrap();
        let draft_save_time = t2.elapsed();
        let draft_path = temp_dir.join("drafts").join("800.draft");
        let draft_file_size = std::fs::metadata(&draft_path).unwrap().len();
        println!(
            "[BENCH 800MB] Save draft (3-locations): {draft_save_time:?} (draft file size: {draft_file_size} bytes)"
        );

        // 6. 単体性能と実機E2E性能の両方を個別に測定（将来のボトルネック特定のため）
        let t_open_only = std::time::Instant::now();
        let reopened = application
            .open_draft(Some(temp_dir.clone()), "bench800".into())
            .unwrap();
        let open_only_time = t_open_only.elapsed();
        println!("[BENCH 800MB] Open draft isolated (Backend restore only): {open_only_time:?}");

        let t_e2e = std::time::Instant::now();
        let drafts = application.read_drafts(Some(temp_dir.clone()));
        assert!(drafts.iter().any(|d| d.id == "800"));
        let reopened_e2e = application
            .open_draft(Some(temp_dir.clone()), "bench800".into())
            .unwrap();
        let first_screen = application.read_lines(reopened_e2e.handle, 0, 50).unwrap();
        assert_eq!(first_screen.lines.len(), 50);
        let e2e_time = t_e2e.elapsed();
        println!(
            "[BENCH 800MB] End-to-End GUI Startup pipeline (read_drafts + open_draft + first 50 lines render): {e2e_time:?}"
        );

        // 7. 編集された3箇所の行が正しく復元されているか検証
        let head_read = application.read_lines(reopened.handle, 100, 1).unwrap();
        assert_eq!(head_read.lines, vec!["EDITED LINE AT HEAD 100 IN 800MB"]);

        let mid_read = application
            .read_lines(reopened.handle, mid_line, 1)
            .unwrap();
        assert_eq!(mid_read.lines, vec!["EDITED LINE AT MID IN 800MB"]);

        let end_read = application
            .read_lines(reopened.handle, end_line, 1)
            .unwrap();
        assert_eq!(end_read.lines, vec!["EDITED LINE AT END IN 800MB"]);

        // 8. Clean な状態（変更なし／Undo 完了後）での下書き保存＆即時行数確定の検証
        application.undo_lines(doc.handle, false).unwrap();
        application.undo_lines(doc.handle, false).unwrap();
        application.undo_lines(doc.handle, false).unwrap();
        application
            .save_draft(
                Some(temp_dir.clone()),
                doc.handle,
                "clean800".into(),
                Some(file_path.to_str().unwrap().into()),
            )
            .unwrap();
        let t_clean_open = std::time::Instant::now();
        let reopened_clean = application
            .open_draft(Some(temp_dir.clone()), "clean800".into())
            .unwrap();
        let clean_open_time = t_clean_open.elapsed();
        println!(
            "[BENCH 800MB] Open CLEAN draft (0-sec line count confirmation): {clean_open_time:?}"
        );
        assert_eq!(reopened_clean.line_count, Some(total_lines));

        println!("[BENCH 800MB] Verification SUCCESS: Head, Mid, End edits and Clean draft restored accurately!");

        application.clear_drafts(Some(temp_dir.clone()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        let _ = std::fs::remove_file(&file_path);
    }

    #[test]
    fn search_job_automatically_cancelled_by_edit_undo_and_new_search() {
        let application = Application::default();
        let doc = application.create_document();
        application
            .replace_lines(
                doc.handle,
                0,
                0,
                vec![
                    "line 1 target".into(),
                    "line 2 target".into(),
                    "line 3 target".into(),
                ],
                1,
                "".into(),
                "".into(),
            )
            .unwrap();

        // 1. 編集によって先行検索が自動キャンセルされること
        let job_before_edit = application
            .prepare_search(doc.handle, "target".into(), false, true, '$', 0, 3, None, true)
            .unwrap();
        application
            .replace_lines(
                doc.handle,
                0,
                1,
                vec!["line 1 modified".into()],
                2,
                "".into(),
                "".into(),
            )
            .unwrap();
        let res1 = job_before_edit.run().unwrap();
        assert!(res1.cancelled, "編集後の先行検索は自動キャンセルされること");

        // 2. Undo によって先行検索が自動キャンセルされること
        let job_before_undo = application
            .prepare_search(doc.handle, "target".into(), false, true, '$', 0, 3, None, true)
            .unwrap();
        application.undo_lines(doc.handle, false).unwrap();
        let res2 = job_before_undo.run().unwrap();
        assert!(res2.cancelled, "Undo後の先行検索は自動キャンセルされること");

        // 3. 新規検索によって先行検索が自動キャンセルされること
        let job_first = application
            .prepare_search(doc.handle, "target".into(), false, true, '$', 0, 3, None, true)
            .unwrap();
        let job_second = application
            .prepare_search(doc.handle, "target".into(), false, true, '$', 0, 3, None, true)
            .unwrap();
        let res_first = job_first.run().unwrap();
        assert!(
            res_first.cancelled,
            "旧検索は新検索の開始により自動キャンセルされること"
        );
        let res_second = job_second.run().unwrap();
        assert!(!res_second.cancelled, "最新の検索は正常完了すること");
        assert_eq!(res_second.hits.len(), 3);
    }
}
