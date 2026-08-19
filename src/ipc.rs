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
struct WriteArgs<'a> {
    path: &'a str,
    contents: &'a str,
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
    pub chars: usize,
}

/// 文書を全文の文字列で受け取らずに開きます。ネイティブ側が行で保持します。
pub async fn open_document(path: &str) -> Result<OpenedDocument, String> {
    let value = call("open_document", PathArg { path }).await?;
    serde_wasm_bindgen::from_value(value).map_err(|e| e.to_string())
}

pub async fn read_lines(handle: u64, from: usize, count: usize) -> Result<Vec<String>, String> {
    #[derive(Serialize)]
    struct Args {
        handle: u64,
        from: usize,
        count: usize,
    }
    let value = call("read_lines", Args { handle, from, count }).await?;
    serde_wasm_bindgen::from_value(value).map_err(|e| e.to_string())
}

pub async fn close_document(handle: u64) {
    #[derive(Serialize)]
    struct HandleArg {
        handle: u64,
    }
    let _ = call("close_document", HandleArg { handle }).await;
}

pub async fn write_document(path: &str, contents: &str) -> Result<(), String> {
    call("write_document", WriteArgs { path, contents })
        .await
        .map(|_| ())
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

pub async fn write_draft(id: usize, path: Option<&str>, contents: &str) {
    #[derive(Serialize)]
    struct DraftArgs<'a> {
        id: String,
        path: Option<&'a str>,
        contents: &'a str,
    }
    let _ = call(
        "write_draft",
        DraftArgs {
            id: id.to_string(),
            path,
            contents,
        },
    )
    .await;
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
