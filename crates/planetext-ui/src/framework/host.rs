//! WebView Host bridge (ファイル ダイアログとディスク アクセス) を呼び出します。

use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

use super::{GuiError, GuiEvent, GuiFramework, MenuState};

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = ["window", "__PLANETEXT_HOST__", "core"], catch)]
    async fn invoke(cmd: &str, args: JsValue) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(js_namespace = ["window", "__PLANETEXT_HOST__", "event"], catch)]
    async fn listen(event: &str, handler: &JsValue) -> Result<JsValue, JsValue>;
}

pub(super) struct HostFramework;
pub(super) static GUI: HostFramework = HostFramework;

async fn call<T: Serialize>(command: &str, args: T) -> Result<JsValue, GuiError> {
    let args = serde_wasm_bindgen::to_value(&args).map_err(|e| e.to_string())?;
    invoke(command, args)
        .await
        .map_err(|error| error_message(&error))
}

fn error_message(value: &JsValue) -> String {
    value
        .as_string()
        .unwrap_or_else(|| "予期しないエラーが発生しました".to_string())
}

#[derive(Serialize)]
struct NoArgs {}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DefaultName<'a> {
    default_name: &'a str,
}

#[derive(Serialize)]
struct PathArg<'a> {
    path: &'a str,
}

#[derive(Serialize)]
struct DirtyArg {
    dirty: bool,
}

#[derive(Serialize)]
struct MessageArg<'a> {
    message: &'a str,
}

impl GuiFramework for HostFramework {
    async fn pick_open_file(&self) -> Result<Option<String>, GuiError> {
        Ok(call("pick_open_path", NoArgs {}).await?.as_string())
    }

    async fn pick_save_file(&self, default_name: &str) -> Result<Option<String>, GuiError> {
        Ok(call("pick_save_path", DefaultName { default_name })
            .await?
            .as_string())
    }

    /// 未保存の作業を破棄してもよいかどうかをユーザーに尋ねます。
    async fn confirm(&self, message: &str) -> Result<bool, GuiError> {
        Ok(call("confirm_discard", MessageArg { message })
            .await?
            .as_bool()
            .unwrap_or(false))
    }

    /// Tells the system's 表示 menu what is currently on.
    async fn set_menu(&self, state: MenuState) -> Result<(), GuiError> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            wrap: bool,
            line_numbers: bool,
            show_whitespace: bool,
            split: bool,
        }
        call(
            "sync_view_menu",
            Args {
                wrap: state.wrap,
                line_numbers: state.line_numbers,
                show_whitespace: state.show_whitespace,
                split: state.split,
            },
        )
        .await
        .map(|_| ())
    }

    async fn open_external(&self, target: &str) -> Result<(), GuiError> {
        #[derive(Serialize)]
        struct Args<'a> {
            url: &'a str,
        }
        call("open_external_url", Args { url: target })
            .await
            .map(|_| ())
    }

    async fn ready(&self) -> Result<(), GuiError> {
        call("frontend_ready", NoArgs {}).await.map(|_| ())
    }

    /// システムのメニューから選択した項目の名前で `chosen` を呼び出します。
    fn on_event(&self, handler: Box<dyn Fn(GuiEvent) + 'static>) -> Result<(), GuiError> {
        let callback = Closure::<dyn FnMut(JsValue)>::new(move |event: JsValue| {
            let name = js_sys::Reflect::get(&event, &JsValue::from_str("payload"))
                .ok()
                .and_then(|payload| payload.as_string());
            if let Some(name) = name {
                handler(GuiEvent::MenuSelected(name));
            }
        });
        wasm_bindgen_futures::spawn_local(async move {
            let _ = listen("menu", callback.as_ref()).await;
            // 維持します: リスナーはウィンドウが存続する限り存続します。
            callback.forget();
        });
        Ok(())
    }
}

/// 範囲読みで開いた文書。行は [`read_lines`] で取り寄せる。
#[derive(Deserialize, Debug, Clone)]
pub struct OpenedDocument {
    pub handle: u64,
    pub line_count: Option<usize>,
    pub bytes: usize,
    #[serde(default = "default_encoding")]
    pub encoding: String,
    #[serde(default = "default_line_ending")]
    pub line_ending: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub revision: u64,
    #[serde(default)]
    pub clean: bool,
}

fn default_encoding() -> String {
    "UTF-8".to_string()
}

fn default_line_ending() -> String {
    "CRLF".to_string()
}

#[derive(Deserialize, Debug, Clone)]
pub struct ReopenedDocument {
    pub line_count: Option<usize>,
    pub encoding: String,
    pub line_ending: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub revision: u64,
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ReadLines {
    pub lines: Vec<String>,
    pub from: usize,
    pub revision: u64,
}

impl std::ops::Deref for ReadLines {
    type Target = [String];
    fn deref(&self) -> &Self::Target {
        &self.lines
    }
}

impl IntoIterator for ReadLines {
    type Item = String;
    type IntoIter = std::vec::IntoIter<String>;
    fn into_iter(self) -> Self::IntoIter {
        self.lines.into_iter()
    }
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct EditApplied {
    pub line_count: usize,
    pub revision: u64,
}

/// 文書を全文の文字列で受け取らずに開きます。ネイティブ側が本体を保持します。
pub async fn open_document(path: &str) -> Result<OpenedDocument, String> {
    let value = call("open_document", PathArg { path }).await?;
    serde_wasm_bindgen::from_value(value).map_err(|e| e.to_string())
}

/// 指定した文字コードで文書を開き直します。
pub async fn reopen_document_encoding(
    handle: u64,
    encoding: &str,
) -> Result<ReopenedDocument, String> {
    #[derive(Serialize)]
    struct Args<'a> {
        handle: u64,
        encoding: &'a str,
    }
    let value = call("reopen_document_encoding", Args { handle, encoding }).await?;
    serde_wasm_bindgen::from_value(value).map_err(|e| e.to_string())
}

/// 文書の文字コードを設定します（保存時に使われます）。
pub async fn set_document_encoding(handle: u64, encoding: &str) -> Result<(), String> {
    #[derive(Serialize)]
    struct Args<'a> {
        handle: u64,
        encoding: &'a str,
    }
    call("set_document_encoding", Args { handle, encoding })
        .await
        .map(|_| ())
}

/// 文書の改行コードを設定します（保存時に使われます）。
pub async fn set_document_line_ending(handle: u64, line_ending: &str) -> Result<(), String> {
    #[derive(Serialize)]
    struct Args<'a> {
        handle: u64,
        line_ending: &'a str,
    }
    call(
        "set_document_line_ending",
        Args {
            handle,
            line_ending,
        },
    )
    .await
    .map(|_| ())
}

/// 走査の完了を待ち、確定した行数を返します。
pub async fn finish_document(handle: u64) -> Result<usize, String> {
    #[derive(Serialize)]
    struct Args {
        handle: u64,
    }
    let value = call("finish_document", Args { handle }).await?;
    serde_wasm_bindgen::from_value(value).map_err(|e| e.to_string())
}

/// 新しい空の文書をネイティブ側のストアに作ります。
pub async fn create_document() -> Option<OpenedDocument> {
    let value = call("create_document", NoArgs {}).await.ok()?;
    serde_wasm_bindgen::from_value(value).ok()
}

pub async fn create_document_from_draft(lines: &[String]) -> Result<OpenedDocument, String> {
    #[derive(Serialize)]
    struct Args<'a> {
        lines: &'a [String],
    }
    let value = call("create_document_from_draft", Args { lines }).await?;
    serde_wasm_bindgen::from_value(value).map_err(|e| e.to_string())
}

/// 下書きIDから文書を復元します。元ファイルがある場合は全文ダンプではなく
/// 元ファイルを開いて未保存差分を適用するため、巨大ファイルでも一瞬で安全に復旧できます。
pub async fn open_draft(id: &str) -> Result<OpenedDocument, String> {
    #[derive(Serialize)]
    struct Args<'a> {
        id: &'a str,
    }
    let value = call("open_draft", Args { id }).await?;
    serde_wasm_bindgen::from_value(value).map_err(|e| e.to_string())
}

pub async fn read_lines(handle: u64, from: usize, count: usize) -> Result<ReadLines, String> {
    #[derive(Serialize)]
    struct Args {
        handle: u64,
        from: usize,
        count: usize,
    }
    let value = call(
        "read_lines",
        Args {
            handle,
            from,
            count,
        },
    )
    .await?;
    serde_wasm_bindgen::from_value(value).map_err(|e| e.to_string())
}

pub async fn read_tail(handle: u64, count: usize) -> Result<ReadLines, String> {
    #[derive(Serialize)]
    struct Args {
        handle: u64,
        count: usize,
    }
    let value = call("read_tail", Args { handle, count }).await?;
    serde_wasm_bindgen::from_value(value).map_err(|e| e.to_string())
}

pub async fn close_document(handle: u64) {
    #[derive(Serialize)]
    struct HandleArg {
        handle: u64,
    }
    let _ = call("close_document", HandleArg { handle }).await;
}

/// 編集の到着: 文書の本体の `from..to` の行を `lines` へ置き換えます。
#[allow(clippy::too_many_arguments)]
pub async fn replace_lines(
    handle: u64,
    from: usize,
    to: usize,
    lines: &[String],
    group: u64,
    before: &str,
    after: &str,
    base_revision: Option<u64>,
) -> Result<EditApplied, String> {
    #[derive(Serialize)]
    struct Args<'a> {
        handle: u64,
        from: usize,
        to: usize,
        lines: &'a [String],
        group: u64,
        before: &'a str,
        after: &'a str,
        base_revision: Option<u64>,
    }
    let value = call(
        "replace_lines",
        Args {
            handle,
            from,
            to,
            lines,
            group,
            before,
            after,
            base_revision,
        },
    )
    .await?;
    serde_wasm_bindgen::from_value(value).map_err(|e| e.to_string())
}

/// 元に戻す・やり直すの結果。`state` は預けたキャレットの控えそのもの。
#[derive(Deserialize)]
pub struct RestoredLines {
    pub state: String,
    pub touched_from: usize,
    pub line_count: usize,
    #[serde(default)]
    pub clean: bool,
    #[serde(default)]
    pub modified_lines: Vec<usize>,
}

pub async fn undo_lines(handle: u64, redo: bool) -> Option<RestoredLines> {
    #[derive(Serialize)]
    struct Args {
        handle: u64,
        redo: bool,
    }
    let value = call("undo_lines", Args { handle, redo }).await.ok()?;
    serde_wasm_bindgen::from_value::<Option<RestoredLines>>(value).ok()?
}

/// 文書の本体からディスクへ直接保存します。全文は webview を通りません。
pub async fn save_document(handle: u64, path: &str) -> Result<(), String> {
    #[derive(Serialize)]
    struct Args<'a> {
        handle: u64,
        path: &'a str,
    }
    call("save_document", Args { handle, path })
        .await
        .map(|_| ())
}

/// 範囲内で `needle` を含む行の番号。読み替えの必要な行を探すのに使います。
pub async fn lines_containing(
    handle: u64,
    from: usize,
    to: usize,
    needle: char,
) -> Result<Vec<usize>, String> {
    #[derive(Serialize)]
    struct Args {
        handle: u64,
        from: usize,
        to: usize,
        needle: char,
    }
    let value = call(
        "lines_containing",
        Args {
            handle,
            from,
            to,
            needle,
        },
    )
    .await?;
    serde_wasm_bindgen::from_value(value).map_err(|e| e.to_string())
}

/// 選択された範囲を本体で組み立てて、システムのクリップボードへ置きます。
pub async fn copy_range(
    handle: u64,
    from: usize,
    first: Option<&str>,
    to: usize,
    last: Option<&str>,
    overrides: &[(usize, String)],
) -> Result<(), String> {
    #[derive(Serialize)]
    struct Args<'a> {
        handle: u64,
        from: usize,
        first: Option<&'a str>,
        to: usize,
        last: Option<&'a str>,
        overrides: &'a [(usize, String)],
    }
    call(
        "copy_range",
        Args {
            handle,
            from,
            first,
            to,
            last,
            overrides,
        },
    )
    .await
    .map(|_| ())
}

/// 検索走査の 1 件。`notation` の行は一致ではなく「手元で見るべき行」。
#[derive(Deserialize)]
pub struct ScanHit {
    pub line: usize,
    pub notation: bool,
    pub start: usize,
    pub end: usize,
}

#[derive(Deserialize)]
pub struct SearchPage {
    pub hits: Vec<ScanHit>,
    pub scanned_to: usize,
    pub cancelled: bool,
    #[serde(default)]
    pub total_matches: Option<usize>,
    #[serde(default)]
    pub current_index: Option<usize>,
}

/// 空のページをnative側で読み進め、最初の候補群までを返します。
#[allow(clippy::too_many_arguments)]
pub async fn search_document(
    handle: u64,
    query: &str,
    regex: bool,
    case_sensitive: bool,
    needle: char,
    from: usize,
    end: usize,
    after_col: Option<usize>,
) -> Result<SearchPage, String> {
    #[derive(Serialize)]
    struct Args<'a> {
        handle: u64,
        query: &'a str,
        regex: bool,
        case_sensitive: bool,
        needle: char,
        from: usize,
        end: usize,
        after_col: Option<usize>,
    }
    let value = call(
        "search_document",
        Args {
            handle,
            query,
            regex,
            case_sensitive,
            needle,
            from,
            end,
            after_col,
        },
    )
    .await?;
    serde_wasm_bindgen::from_value(value).map_err(|e| e.to_string())
}

#[allow(dead_code)]
pub async fn cancel_search(handle: u64) {
    #[derive(Serialize)]
    struct Args {
        handle: u64,
    }
    let _ = call("cancel_search", Args { handle }).await;
}

/// 等間隔の標本から全文のおよその一致数を返します。
pub async fn estimate_matches(
    handle: u64,
    query: &str,
    regex: bool,
    case_sensitive: bool,
) -> Result<usize, String> {
    #[derive(Serialize)]
    struct Args<'a> {
        handle: u64,
        query: &'a str,
        regex: bool,
        case_sensitive: bool,
    }
    let value = call(
        "estimate_matches",
        Args {
            handle,
            query,
            regex,
            case_sensitive,
        },
    )
    .await?;
    serde_wasm_bindgen::from_value(value).map_err(|e| e.to_string())
}

/// 文書の本体から下書きを書きます。
pub async fn save_draft(handle: u64, id: usize, path: Option<&str>) {
    #[derive(Serialize)]
    struct Args<'a> {
        handle: u64,
        id: String,
        path: Option<&'a str>,
    }
    let _ = call(
        "save_draft",
        Args {
            handle,
            id: id.to_string(),
            path,
        },
    )
    .await;
}

pub async fn set_dirty(dirty: bool) {
    let _ = call("set_dirty", DirtyArg { dirty }).await;
}

/// 保存された設定 (ファイルのテキストとして)。まだ何もない場合は空です。
pub async fn read_settings() -> String {
    call("read_settings", NoArgs {})
        .await
        .ok()
        .and_then(|value| value.as_string())
        .unwrap_or_default()
}

pub async fn write_settings(contents: &str) {
    #[derive(Serialize)]
    struct ContentsArg<'a> {
        contents: &'a str,
    }
    let _ = call("write_settings", ContentsArg { contents }).await;
}

/// 前回画面に表示されていたもの、保存されているかどうか。
pub struct Draft {
    pub id: usize,
    pub path: Option<String>,
    pub contents: String,
    pub clean: bool,
}

pub async fn remove_draft(id: usize) {
    #[derive(Serialize)]
    struct IdArg {
        id: String,
    }
    let _ = call("remove_draft", IdArg { id: id.to_string() }).await;
}

pub async fn read_drafts() -> Vec<Draft> {
    #[derive(Deserialize)]
    struct Raw {
        id: String,
        path: Option<String>,
        contents: String,
        #[serde(default)]
        clean: bool,
    }
    let Ok(value) = call("read_drafts", NoArgs {}).await else {
        return Vec::new();
    };
    let raw: Vec<Raw> = serde_wasm_bindgen::from_value(value).unwrap_or_default();
    raw.into_iter()
        .filter_map(|raw| {
            Some(Draft {
                id: raw.id.parse().ok()?,
                path: raw.path,
                contents: raw.contents,
                clean: raw.clean,
            })
        })
        .collect()
}

pub async fn save_session_state(state_json: &str) {
    #[derive(Serialize)]
    struct StateArg<'a> {
        state_json: &'a str,
    }
    let _ = call("save_session_state", StateArg { state_json }).await;
}

pub async fn read_session_state() -> Option<String> {
    let value = call("read_session_state", NoArgs {}).await.ok()?;
    value.as_string()
}

/// 開いている文書のファイルサイズを返します。保存済みならそのファイル、未保存なら下書き
/// ファイルのサイズです。どちらも読めないときは `None` を返します（エラーにはしません）。
pub async fn file_size(path: Option<&str>, id: usize) -> Option<usize> {
    #[derive(Serialize)]
    struct FileSizeArgs<'a> {
        path: Option<&'a str>,
        id: String,
    }
    let value = call(
        "file_size",
        FileSizeArgs {
            path,
            id: id.to_string(),
        },
    )
    .await
    .ok()?;
    value.as_f64().map(|n| n as usize)
}
