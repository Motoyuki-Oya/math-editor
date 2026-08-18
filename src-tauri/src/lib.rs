mod menu;

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

/// 設定ファイルが存在する場所: アプリ独自の構成ディレクトリ内の `settings.toml`。
fn settings_path(app: &tauri::AppHandle) -> Option<PathBuf> {
    let dir = app.path().app_config_dir().ok()?;
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("settings.toml"))
}

#[tauri::command]
fn read_settings(app: tauri::AppHandle) -> String {
    settings_path(&app)
        .and_then(|path| std::fs::read_to_string(path).ok())
        .unwrap_or_default()
}

#[tauri::command]
fn write_settings(app: tauri::AppHandle, contents: String) -> Result<(), String> {
    let Some(path) = settings_path(&app) else {
        return Err("設定の保存先がありません".to_string());
    };
    std::fs::write(&path, contents).map_err(|e| format!("設定を保存できませんでした: {e}"))
}

/// 下書きが存在する場所: 開いているドキュメントごとに、設定の横に 1 つのファイル。
///
/// 下書きは、保存されているかどうかに関係なく、現在画面上に表示されているものです。ドキュメント自体のファイルは、ユーザーが保存するときにのみ書き込まれるため、ユーザーがファイル自体に触れることがない限り、クラッシュや停電によるコストは発生しません。
fn drafts_dir(app: &tauri::AppHandle) -> Option<PathBuf> {
    let dir = app.path().app_config_dir().ok()?.join("drafts");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// 下書きファイルは最初の行にドキュメントのパスがあり、その後にドキュメント自体が含まれているため、復元されたドラフトではそれがどのファイルに属しているかがわかります。
#[derive(serde::Serialize)]
struct Draft {
    id: String,
    path: Option<String>,
    contents: String,
}

#[tauri::command]
fn write_draft(
    app: tauri::AppHandle,
    id: String,
    path: Option<String>,
    contents: String,
) -> Result<(), String> {
    let Some(dir) = drafts_dir(&app) else {
        return Err("下書きの保存先がありません".to_string());
    };
    let file = format!("{}\n{contents}", path.unwrap_or_default());
    std::fs::write(dir.join(draft_name(&id)), file)
        .map_err(|e| format!("下書きを保存できませんでした: {e}"))
}

#[tauri::command]
fn file_size(app: tauri::AppHandle, path: Option<String>, id: String) -> Result<u64, String> {
    let target = match path {
        Some(p) => PathBuf::from(p),
        None => {
            let Some(dir) = drafts_dir(&app) else {
                return Err("下書きの保存先がありません".to_string());
            };
            dir.join(draft_name(&id))
        }
    };
    std::fs::metadata(&target)
        .map(|m| m.len())
        .map_err(|e| format!("ファイルサイズを取得できませんでした: {e}"))
}

#[tauri::command]
fn remove_draft(app: tauri::AppHandle, id: String) {
    if let Some(dir) = drafts_dir(&app) {
        std::fs::remove_file(dir.join(draft_name(&id))).ok();
    }
}

#[tauri::command]
fn read_drafts(app: tauri::AppHandle) -> Vec<Draft> {
    let Some(dir) = drafts_dir(&app) else {
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

fn clear_drafts(app: &tauri::AppHandle) {
    if let Some(dir) = drafts_dir(app) {
        std::fs::remove_dir_all(dir).ok();
    }
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

/// 設定の横に、実行全体のウィンドウ サイズを記憶します。
fn window_size_path(app: &tauri::AppHandle) -> Option<PathBuf> {
    let dir = app.path().app_config_dir().ok()?;
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("window.toml"))
}

fn save_window_size(window: &tauri::Window) {
    let Some(path) = window_size_path(window.app_handle()) else {
        return;
    };
    if let Ok(size) = window.inner_size() {
        if size.width > 0 && size.height > 0 {
            let contents = format!("width = {}\nheight = {}\n", size.width, size.height);
            std::fs::write(path, contents).ok();
        }
    }
}

fn restore_window_size(app: &tauri::AppHandle) {
    let Some(contents) = window_size_path(app).and_then(|path| std::fs::read_to_string(path).ok())
    else {
        return;
    };
    let mut width = None;
    let mut height = None;
    for line in contents.lines() {
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        match name.trim() {
            "width" => width = value.trim().parse::<u32>().ok(),
            "height" => height = value.trim().parse::<u32>().ok(),
            _ => {}
        }
    }
    if let (Some(width), Some(height)) = (width, height) {
        if let Some(window) = app.get_webview_window("main") {
            window.set_size(tauri::PhysicalSize { width, height }).ok();
        }
    }
}

#[tauri::command]
fn set_dirty(state: State<'_, AppState>, dirty: bool) {
    *state.dirty.lock().unwrap() = dirty;
}

/// `PLANETEXT_STARTUP_LOG` が設定されている場合、プロセスの開始から最初のフロントエンド ペイントまでの時間を報告します。起動コストを監視するために使用されます。
#[tauri::command]
fn frontend_ready(state: State<'_, AppState>) {
    if std::env::var_os("PLANETEXT_STARTUP_LOG").is_some() {
        eprintln!(
            "planetext startup: {} ms",
            state.started.elapsed().as_millis()
        );
    }
}

/// 保存されていない作業が失われる前に確認します。 WebView 自体の `confirm()` はすべてのプラットフォームで使用できるわけではないため、質問はネイティブ ダイアログを経由します。
#[tauri::command]
async fn confirm_discard(app: tauri::AppHandle, message: String) -> bool {
    let (tx, rx) = std::sync::mpsc::channel();
    app.dialog()
        .message(message)
        .title("Planetext")
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
        .title("Planetext")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "破棄して終了".into(),
            "キャンセル".into(),
        ))
        .show(move |discard| {
            if discard {
                // 意図的に破棄されます。復元するものは何もありません。
                clear_drafts(target.app_handle());
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
        .setup(|app| {
            restore_window_size(app.handle());
            menu::install(app.handle())?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                save_window_size(window);
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
            confirm_discard,
            read_settings,
            write_settings,
            write_draft,
            remove_draft,
            read_drafts,
            file_size,
            menu::sync_view_menu
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
