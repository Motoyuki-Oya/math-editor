use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

use tauri::{Manager, State, WindowEvent};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

struct AppState {
    dirty: Mutex<bool>,
    started: Instant,
}

fn pick_path<F>(run: F) -> Option<PathBuf>
where
    F: FnOnce(Box<dyn FnOnce(Option<tauri_plugin_dialog::FilePath>) + Send>),
{
    let (tx, rx) = std::sync::mpsc::channel();
    run(Box::new(move |path| {
        let _ = tx.send(path);
    }));
    rx.recv().ok().flatten().and_then(|p| p.into_path().ok())
}

#[tauri::command]
async fn pick_open_path(app: tauri::AppHandle) -> Option<String> {
    pick_path(|cb| {
        app.dialog()
            .file()
            .set_title("開く")
            .add_filter(
                "テキスト",
                &["txt", "md", "markdown", "text", "log", "csv", "tex"],
            )
            .add_filter("すべてのファイル", &["*"])
            .pick_file(cb);
    })
    .map(|p| p.to_string_lossy().into_owned())
}

#[tauri::command]
async fn pick_save_path(app: tauri::AppHandle, default_name: String) -> Option<String> {
    pick_path(|cb| {
        app.dialog()
            .file()
            .set_title("名前を付けて保存")
            .set_file_name(default_name)
            .add_filter("テキスト", &["txt"])
            .add_filter("Markdown", &["md"])
            .add_filter("すべてのファイル", &["*"])
            .save_file(cb);
    })
    .map(|p| p.to_string_lossy().into_owned())
}

#[tauri::command]
fn read_document(path: String) -> Result<String, String> {
    std::fs::read_to_string(&path).map_err(|e| format!("{path} を読み込めませんでした: {e}"))
}

#[tauri::command]
fn write_document(path: String, contents: String) -> Result<(), String> {
    std::fs::write(&path, contents).map_err(|e| format!("{path} を保存できませんでした: {e}"))
}

#[tauri::command]
fn set_dirty(state: State<'_, AppState>, dirty: bool) {
    *state.dirty.lock().unwrap() = dirty;
}

/// Reports the time from process start to the first frontend paint when
/// `MATHNOTE_STARTUP_LOG` is set. Used to keep an eye on startup cost.
#[tauri::command]
fn frontend_ready(state: State<'_, AppState>) {
    if std::env::var_os("MATHNOTE_STARTUP_LOG").is_some() {
        eprintln!(
            "mathnote startup: {} ms",
            state.started.elapsed().as_millis()
        );
    }
}

/// Asks before losing unsaved work. The WebView's own `confirm()` is not
/// usable on every platform, so the question goes through the native dialog.
#[tauri::command]
async fn confirm_discard(app: tauri::AppHandle, message: String) -> bool {
    let (tx, rx) = std::sync::mpsc::channel();
    app.dialog()
        .message(message)
        .title("MathNote")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "破棄する".into(),
            "キャンセル".into(),
        ))
        .show(move |discard| {
            let _ = tx.send(discard);
        });
    rx.recv().unwrap_or(false)
}

fn is_dirty(window: &tauri::Window) -> bool {
    window
        .state::<AppState>()
        .dirty
        .lock()
        .map(|dirty| *dirty)
        .unwrap_or(false)
}

fn confirm_discard_on_close(window: &tauri::Window) {
    let target = window.clone();
    window
        .dialog()
        .message("保存されていない変更があります。破棄して終了しますか？")
        .title("MathNote")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "破棄して終了".into(),
            "キャンセル".into(),
        ))
        .show(move |discard| {
            if discard {
                if let Ok(mut dirty) = target.state::<AppState>().dirty.lock() {
                    *dirty = false;
                }
                let _ = target.destroy();
            }
        });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState {
            dirty: Mutex::new(false),
            started: Instant::now(),
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                if is_dirty(window) {
                    api.prevent_close();
                    confirm_discard_on_close(window);
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            pick_open_path,
            pick_save_path,
            read_document,
            write_document,
            set_dirty,
            frontend_ready,
            confirm_discard
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
