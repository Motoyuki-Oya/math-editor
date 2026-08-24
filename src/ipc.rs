//! Tauri バックエンド (ファイル ダイアログとディスク アクセス) を呼び出します。

use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "core"], catch)]
    async fn invoke(cmd: &str, args: JsValue) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "event"], catch)]
    async fn listen(event: &str, handler: &JsValue) -> Result<JsValue, JsValue>;
}

/// システムのメニューから選択した項目の名前で `chosen` を呼び出します。
pub fn on_menu(chosen: impl Fn(&str) + 'static) {
    let handler = Closure::<dyn FnMut(JsValue)>::new(move |event: JsValue| {
        let name = js_sys::Reflect::get(&event, &JsValue::from_str("payload"))
            .ok()
            .and_then(|payload| payload.as_string());
        if let Some(name) = name {
            chosen(&name);
        }
    });
    wasm_bindgen_futures::spawn_local(async move {
        let _ = listen("menu", handler.as_ref()).await;
        // 維持します: リスナーはウィンドウが存続する限り存続します。
        handler.forget();
    });
}

/// Tells the system's 表示 menu what is currently on.
pub async fn sync_view_menu(wrap: bool, line_numbers: bool, split: bool) {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Args {
        wrap: bool,
        line_numbers: bool,
        split: bool,
    }
    let _ = call(
        "sync_view_menu",
        Args {
            wrap,
            line_numbers,
            split,
        },
    )
    .await;
}

async fn call<T: Serialize>(command: &str, args: T) -> Result<JsValue, String> {
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

/// 未保存の作業を破棄してもよいかどうかをユーザーに尋ねます。
pub async fn confirm_discard(message: &str) -> bool {
    call("confirm_discard", MessageArg { message })
        .await
        .ok()
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub async fn pick_open_path() -> Option<String> {
    call("pick_open_path", NoArgs {}).await.ok()?.as_string()
}

pub async fn pick_save_path(default_name: &str) -> Option<String> {
    call("pick_save_path", DefaultName { default_name })
        .await
        .ok()?
        .as_string()
}

/// 範囲読みで開いた文書。行は [`read_lines`] で取り寄せる。
#[derive(Deserialize)]
pub struct OpenedDocument {
    pub handle: u64,
    pub line_count: usize,
    pub bytes: usize,
}

/// 文書を全文の文字列で受け取らずに開きます。ネイティブ側が本体を保持します。
pub async fn open_document(path: &str) -> Result<OpenedDocument, String> {
    let value = call("open_document", PathArg { path }).await?;
    serde_wasm_bindgen::from_value(value).map_err(|e| e.to_string())
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

pub async fn read_lines(handle: u64, from: usize, count: usize) -> Result<Vec<String>, String> {
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

pub async fn close_document(handle: u64) {
    #[derive(Serialize)]
    struct HandleArg {
        handle: u64,
    }
    let _ = call("close_document", HandleArg { handle }).await;
}

/// 編集の到着: 文書の本体の `from..to` の行を `lines` へ置き換えます。
pub async fn replace_lines(
    handle: u64,
    from: usize,
    to: usize,
    lines: &[String],
    group: u64,
    before: &str,
    after: &str,
) -> Result<(), String> {
    #[derive(Serialize)]
    struct Args<'a> {
        handle: u64,
        from: usize,
        to: usize,
        lines: &'a [String],
        group: u64,
        before: &'a str,
        after: &'a str,
    }
    call(
        "replace_lines",
        Args {
            handle,
            from,
            to,
            lines,
            group,
            before,
            after,
        },
    )
    .await
    .map(|_| ())
}

/// 元に戻す・やり直すの結果。`state` は預けたキャレットの控えそのもの。
#[derive(Deserialize)]
pub struct RestoredLines {
    pub state: String,
    pub touched_from: usize,
    pub line_count: usize,
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
            })
        })
        .collect()
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

pub async fn frontend_ready() {
    let _ = call("frontend_ready", NoArgs {}).await;
}
