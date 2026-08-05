//! Calls into the Tauri backend (file dialogs and disk access).

use serde::Serialize;
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

pub async fn pick_open_path() -> Option<String> {
    call("pick_open_path", NoArgs {}).await.ok()?.as_string()
}

pub async fn pick_save_path(default_name: &str) -> Option<String> {
    call("pick_save_path", DefaultName { default_name })
        .await
        .ok()?
        .as_string()
}

pub async fn pick_export_path(default_name: &str) -> Option<String> {
    call("pick_export_path", DefaultName { default_name })
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

pub async fn frontend_ready() {
    let _ = call("frontend_ready", NoArgs {}).await;
}
