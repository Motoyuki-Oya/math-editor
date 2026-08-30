mod store;

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
    docs: Mutex<HashMap<u64, store::Document>>,
    /// 文書ごとの検索世代。値が変われば走査スレッドは古い検索を中止する。
    searches: Mutex<HashMap<u64, Arc<AtomicU64>>>,
    next_document: Mutex<u64>,
}

/// 開き方の答え: 文書の取っ手と、行数と大きさ、文字コード、改行コード。
#[derive(serde::Serialize)]
pub struct OpenedDocument {
    handle: u64,
    line_count: usize,
    bytes: usize,
    encoding: String,
    line_ending: String,
}

#[derive(serde::Serialize)]
pub struct ReopenedDocument {
    line_count: usize,
    encoding: String,
    line_ending: String,
}

/// 元に戻す・やり直すの結果。`state` は frontend が預けた控えそのもの。
#[derive(serde::Serialize)]
pub struct RestoredLines {
    state: String,
    touched_from: usize,
    line_count: usize,
    clean: bool,
}

#[derive(serde::Serialize)]
pub struct SearchPage {
    hits: Vec<store::ScanHit>,
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
    index: Option<Arc<store::ScanIndex>>,
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
    snapshot: store::Document,
    pattern: regex::Regex,
    literal: Option<String>,
    case_sensitive: bool,
    needle: char,
    from: usize,
    end: usize,
    after_col: Option<usize>,
    generation: Arc<AtomicU64>,
    ticket: u64,
}

impl SearchJob {
    pub fn run(mut self) -> Result<SearchPage, String> {
        let found = self.snapshot.search_candidates(
            store::SearchSpec {
                pattern: &self.pattern,
                literal: self.literal.as_deref(),
                case_sensitive: self.case_sensitive,
                marker: self.needle,
                from: self.from,
                end: self.end,
                after_col: self.after_col,
            },
            &|| self.generation.load(Ordering::Relaxed) != self.ticket,
        )?;
        Ok(SearchPage {
            hits: found.hits,
            scanned_to: found.scanned_to,
            cancelled: found.cancelled,
        })
    }
}

pub struct GlobalShortcutSettings {
    pub enabled: bool,
    pub key: String,
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
        f: impl FnOnce(&mut store::Document) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut docs = self.state.docs.lock().unwrap();
        let doc = docs
            .get_mut(&handle)
            .ok_or_else(|| "文書はもう閉じられています".to_string())?;
        f(doc)
    }

    fn adopt(&self, doc: store::Document) -> OpenedDocument {
        let encoding = doc.encoding().label().to_string();
        let line_ending = doc.line_ending().label().to_string();
        let opened = OpenedDocument {
            handle: {
                let mut next = self.state.next_document.lock().unwrap();
                *next += 1;
                *next
            },
            line_count: doc.line_count(),
            bytes: doc.bytes(),
            encoding,
            line_ending,
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
        let (doc, scan) = store::Document::open(&path)?;
        let opened = self.adopt(doc);
        if let Some(scan) = scan {
            std::thread::spawn(move || {
                let _ = scan.run();
            });
        }
        Ok(opened)
    }

    pub fn reopen_document_encoding(
        &self,
        handle: u64,
        encoding: String,
    ) -> Result<ReopenedDocument, String> {
        let enc = store::FileEncoding::from_label(&encoding)
            .ok_or_else(|| format!("未知の文字コードです: {encoding}"))?;
        let (line_count, enc_label, line_ending, scan) = self.with_doc(handle, |doc| {
            let scan = doc.reopen_with_encoding(enc)?;
            Ok((
                doc.line_count(),
                doc.encoding().label().to_string(),
                doc.line_ending().label().to_string(),
                scan,
            ))
        })?;
        if let Some(scan) = scan {
            std::thread::spawn(move || {
                let _ = scan.run();
            });
        }
        Ok(ReopenedDocument {
            line_count,
            encoding: enc_label,
            line_ending,
        })
    }

    pub fn set_document_encoding(&self, handle: u64, encoding: String) -> Result<(), String> {
        let enc = store::FileEncoding::from_label(&encoding)
            .ok_or_else(|| format!("未知の文字コードです: {encoding}"))?;
        self.with_doc(handle, |doc| {
            doc.set_encoding(enc);
            Ok(())
        })
    }

    pub fn set_document_line_ending(&self, handle: u64, line_ending: String) -> Result<(), String> {
        let le = store::LineEnding::from_label(&line_ending)
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
        self.adopt(store::Document::empty())
    }

    pub fn read_lines(
        &self,
        handle: u64,
        from: usize,
        count: usize,
    ) -> Result<Vec<String>, String> {
        self.with_doc(handle, |doc| doc.read(from, count))
    }

    pub fn read_tail(&self, handle: u64, count: usize) -> Result<Vec<String>, String> {
        self.with_doc(handle, |doc| doc.read_tail(count))
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
    ) -> Result<usize, String> {
        self.with_doc(handle, |doc| {
            doc.replace(from, to, lines, group, &before, &after)
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
        let pattern = if regex {
            query.clone()
        } else {
            regex::escape(&query)
        };
        let pattern = regex::RegexBuilder::new(&pattern)
            .case_insensitive(!case_sensitive)
            .build()
            .map_err(|e| format!("正規表現を読めませんでした: {e}"))?;
        let literal = (!regex && (case_sensitive || query.is_ascii())).then_some(query);
        Ok(SearchJob {
            snapshot,
            pattern,
            literal,
            case_sensitive,
            needle,
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
        let pattern = if regex { query } else { regex::escape(&query) };
        let pattern = regex::RegexBuilder::new(&pattern)
            .case_insensitive(!case_sensitive)
            .build()
            .map_err(|e| format!("正規表現を読めませんでした: {e}"))?;
        self.with_doc(handle, |doc| doc.estimate_matches(&pattern))
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
            use std::io::Write;
            writeln!(out, "{}", path.unwrap_or_default())
                .map_err(|e| format!("下書きを保存できませんでした: {e}"))?;
            doc.write_to(&mut out)
                .map_err(|e| format!("下書きを保存できませんでした: {e}"))
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
                if let Ok(meta) = entry.metadata() {
                    if meta.len() > 10 * 1024 * 1024 {
                        return None;
                    }
                }
                let id = path.file_stem()?.to_string_lossy().into_owned();
                let file = std::fs::read_to_string(&path).ok()?;
                let (first, contents) = file.split_once('\n').unwrap_or(("", file.as_str()));
                Some(Draft {
                    id,
                    path: (!first.is_empty()).then(|| first.to_string()),
                    contents: contents.to_string(),
                })
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
    use super::global_shortcut_settings;

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
}
