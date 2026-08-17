//! Calls into the Tauri backend (file dialogs and disk access).

use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "core"], catch)]
    async fn invoke(cmd: &str, args: JsValue) -> Result<JsValue, JsValue>;
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

/// Asks the user whether unsaved work may be thrown away.
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

pub async fn read_document(path: &str) -> Result<String, String> {
    let value = call("read_document", PathArg { path }).await?;
    Ok(value.as_string().unwrap_or_default())
}

pub async fn write_document(path: &str, contents: &str) -> Result<(), String> {
    call("write_document", WriteArgs { path, contents })
        .await
        .map(|_| ())
}

pub async fn set_dirty(dirty: bool) {
    let _ = call("set_dirty", DirtyArg { dirty }).await;
}

/// The saved settings, as the file's text. Empty when there is none yet.
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

/// What was on screen last time, whether it had been saved or not.
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

pub async fn frontend_ready() {
    let _ = call("frontend_ready", NoArgs {}).await;
}
