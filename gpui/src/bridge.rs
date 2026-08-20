//! WebView 内のフロントエンドから `window.ipc.postMessage` で送られてくる呼び出しを処理します。
//! Tauri の `__TAURI__.core.invoke` と `__TAURI__.event.listen` の動作を再現します。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use wry::http::Request;
use wry::WebView;

#[derive(Deserialize)]
struct Rpc {
    id: u64,
    cmd: String,
    #[serde(default)]
    args: Value,
}

#[derive(Serialize)]
struct Reply<T: Serialize> {
    id: u64,
    ok: bool,
    value: T,
}

#[derive(Serialize)]
struct ErrorReply {
    id: u64,
    ok: bool,
    error: String,
}

pub struct IpcHandler {
    webview: Rc<WebView>,
}

impl IpcHandler {
    pub fn new(webview: Rc<WebView>) -> Self {
        Self { webview }
    }

    pub fn handle(&self, request: Request<String>) {
        let body = request.body().clone();
        let reply = match serde_json::from_str::<Rpc>(&body) {
            Ok(rpc) => self.dispatch(rpc),
            Err(e) => Self::reject_text(0, format!("invalid rpc: {e}")),
        };
        let _ = self.webview.evaluate_script(&reply);
    }

    fn dispatch(&self, rpc: Rpc) -> String {
        let result = match rpc.cmd.as_str() {
            "confirm_discard" => confirm_discard(&rpc.args),
            "pick_open_path" => pick_open_path(),
            "pick_save_path" => pick_save_path(&rpc.args),
            "read_document" => read_document(&rpc.args),
            "write_document" => write_document(&rpc.args),
            "set_dirty" => Ok(Value::Null),
            "read_settings" => read_settings(),
            "write_settings" => write_settings(&rpc.args),
            "write_draft" => write_draft(&rpc.args),
            "remove_draft" => remove_draft(&rpc.args),
            "read_drafts" => read_drafts(),
            "file_size" => file_size(&rpc.args),
            "frontend_ready" | "sync_view_menu" => Ok(Value::Null),
            _ => Err(format!("unknown command: {}", rpc.cmd)),
        };

        match result {
            Ok(value) => serde_json::to_string(&Reply {
                id: rpc.id,
                ok: true,
                value,
            })
            .unwrap_or_default(),
            Err(error) => Self::reject_text(rpc.id, error),
        }
    }

    fn reject_text(id: u64, error: String) -> String {
        serde_json::to_string(&ErrorReply {
            id,
            ok: false,
            error,
        })
        .unwrap_or_default()
    }
}

fn arg<T: serde::de::DeserializeOwned>(args: &Value) -> Result<T, String> {
    serde_json::from_value(args.clone()).map_err(|e| e.to_string())
}

fn config_dir() -> Result<PathBuf, String> {
    dirs::config_dir()
        .map(|d| d.join("Planetext"))
        .ok_or_else(|| "設定ディレクトリが取得できません".into())
}

fn ensure_dir(dir: &PathBuf) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| e.to_string())
}

fn confirm_discard(args: &Value) -> Result<Value, String> {
    let message: String = arg(args)?;
    let result = rfd::MessageDialog::new()
        .set_title("Planetext")
        .set_description(&message)
        .set_buttons(rfd::MessageButtons::YesNo)
        .show();
    Ok(Value::Bool(result == rfd::MessageDialogResult::Yes))
}

fn pick_open_path() -> Result<Value, String> {
    let file = rfd::FileDialog::new()
        .add_filter(
            "テキスト",
            &["txt", "md", "markdown", "text", "log", "csv", "tex"],
        )
        .add_filter("すべてのファイル", &["*"])
        .pick_file();
    Ok(file
        .map(|p| Value::String(p.to_string_lossy().into_owned()))
        .unwrap_or(Value::Null))
}

fn pick_save_path(args: &Value) -> Result<Value, String> {
    let args: serde_json::Map<String, Value> = arg(args)?;
    let default = args
        .get("default_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let file = rfd::FileDialog::new()
        .set_file_name(default)
        .add_filter("テキスト", &["txt"])
        .add_filter("Markdown", &["md"])
        .add_filter("すべてのファイル", &["*"])
        .save_file();
    Ok(file
        .map(|p| Value::String(p.to_string_lossy().into_owned()))
        .unwrap_or(Value::Null))
}

fn read_document(args: &Value) -> Result<Value, String> {
    let args: serde_json::Map<String, Value> = arg(args)?;
    let path = args["path"].as_str().ok_or("path missing")?;
    fs::read_to_string(path)
        .map(Value::String)
        .map_err(|e| format!("{path} を読み込めませんでした: {e}"))
}

fn write_document(args: &Value) -> Result<Value, String> {
    let args: serde_json::Map<String, Value> = arg(args)?;
    let path = args["path"].as_str().ok_or("path missing")?;
    let contents = args["contents"].as_str().ok_or("contents missing")?;
    fs::write(path, contents).map_err(|e| format!("{path} を保存できませんでした: {e}"))?;
    Ok(Value::Null)
}

fn read_settings() -> Result<Value, String> {
    let dir = config_dir()?;
    ensure_dir(&dir)?;
    let path = dir.join("settings.toml");
    let text = fs::read_to_string(&path).unwrap_or_default();
    Ok(Value::String(text))
}

fn write_settings(args: &Value) -> Result<Value, String> {
    let args: serde_json::Map<String, Value> = arg(args)?;
    let contents = args["contents"].as_str().ok_or("contents missing")?;
    let dir = config_dir()?;
    ensure_dir(&dir)?;
    let path = dir.join("settings.toml");
    fs::write(&path, contents).map_err(|e| format!("設定を保存できませんでした: {e}"))?;
    Ok(Value::Null)
}

fn drafts_dir() -> Result<PathBuf, String> {
    let dir = config_dir()?.join("drafts");
    ensure_dir(&dir)?;
    Ok(dir)
}

fn write_draft(args: &Value) -> Result<Value, String> {
    let args: serde_json::Map<String, Value> = arg(args)?;
    let id = args["id"].as_str().ok_or("id missing")?;
    let path = args["path"].as_str();
    let contents = args["contents"].as_str().ok_or("contents missing")?;
    let dir = drafts_dir()?;
    let file = format!("{}\n{contents}", path.unwrap_or(""));
    fs::write(dir.join(id), file).map_err(|e| format!("下書きを保存できませんでした: {e}"))?;
    Ok(Value::Null)
}

fn remove_draft(args: &Value) -> Result<Value, String> {
    let args: serde_json::Map<String, Value> = arg(args)?;
    let id = args["id"].as_str().ok_or("id missing")?;
    let dir = drafts_dir()?;
    let _ = fs::remove_file(dir.join(id));
    Ok(Value::Null)
}

#[derive(Serialize)]
struct Draft {
    id: String,
    path: Option<String>,
    contents: String,
}

fn read_drafts() -> Result<Value, String> {
    let dir = drafts_dir()?;
    let mut drafts = Vec::new();
    for entry in fs::read_dir(&dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_type().map_err(|e| e.to_string())?.is_file() {
            continue;
        }
        let id = entry.file_name().to_string_lossy().into_owned();
        let text = fs::read_to_string(entry.path()).unwrap_or_default();
        let mut lines = text.lines();
        let path = lines
            .next()
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty());
        let contents = lines.collect::<Vec<_>>().join("\n");
        drafts.push(Draft { id, path, contents });
    }
    serde_json::to_value(drafts).map_err(|e| e.to_string())
}

fn file_size(args: &Value) -> Result<Value, String> {
    let args: serde_json::Map<String, Value> = arg(args)?;
    let path = args["path"].as_str();
    let id = args["id"].as_str();
    let size = if let Some(p) = path {
        fs::metadata(p).ok().map(|m| m.len() as f64)
    } else if let Some(id) = id {
        drafts_dir()
            .ok()
            .and_then(|d| fs::metadata(d.join(id)).ok().map(|m| m.len() as f64))
    } else {
        None
    };
    Ok(size.map(Value::from).unwrap_or(Value::Null))
}

/// フロントエンドに注入するスクリプト。`__TAURI__.core.invoke` と `__TAURI__.event.listen` を提供します。
pub const INIT_SCRIPT: &str = r#"
(function () {
    if (window.__PLANETEXT_RPC__) return;
    const rpc = {
        promises: {},
        listeners: {},
        nextId: 1,
        invoke: function (cmd, args) {
            return new Promise(function (resolve, reject) {
                const id = rpc.nextId++;
                rpc.promises[id] = { resolve, reject };
                if (window.ipc && window.ipc.postMessage) {
                    window.ipc.postMessage(JSON.stringify({ id, cmd, args }));
                } else {
                    reject(new Error('ipc is not available'));
                }
            });
        },
        resolve: function (id, value) {
            const p = rpc.promises[id];
            if (p) { p.resolve(value); delete rpc.promises[id]; }
        },
        reject: function (id, error) {
            const p = rpc.promises[id];
            if (p) { p.reject(new Error(error)); delete rpc.promises[id]; }
        },
        emit: function (event, payload) {
            const h = rpc.listeners[event];
            if (h) h({ payload });
        },
        listen: function (event, handler) {
            rpc.listeners[event] = handler;
            return Promise.resolve(function () { delete rpc.listeners[event]; });
        }
    };
    window.__PLANETEXT_RPC__ = rpc;
    window.__TAURI__ = {
        core: { invoke: rpc.invoke },
        event: { listen: rpc.listen }
    };
})();
"#;
