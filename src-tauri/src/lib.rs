//! Tauri接続コード(OS側)。変換とdispatchのみ。

mod menu;
mod platform;

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

use planetext_document::{
    global_shortcut_settings, Application, Draft, GlobalShortcutSettings, GuiAction, GuiEvent,
    OpenedDocument, ReopenedDocument, RestoredLines, SearchPage, TrayAction,
};
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Manager, State, WindowEvent};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};

fn debug_log(msg: &str) {
    println!("{}", msg);
}

struct AppState {
    started: Instant,
    tray: Mutex<Option<tauri::tray::TrayIcon>>,
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
                "サポートされているファイル",
                &[
                    "txt", "md", "markdown", "text", "log", "csv", "tex", "rs", "py", "js", "ts",
                    "jsx", "tsx", "html", "htm", "css", "json", "toml", "kt", "kts", "c", "cpp",
                    "h", "hpp",
                ],
            )
            .add_filter("テキスト", &["txt", "text", "log", "csv"])
            .add_filter("Markdown", &["md", "markdown"])
            .add_filter("Rust", &["rs"])
            .add_filter("Python", &["py"])
            .add_filter("TypeScript / JavaScript", &["ts", "js", "tsx", "jsx"])
            .add_filter("HTML / CSS", &["html", "htm", "css"])
            .add_filter("JSON / TOML", &["json", "toml"])
            .add_filter("Kotlin", &["kt", "kts"])
            .add_filter("LaTeX", &["tex"])
            .add_filter("C / C++", &["c", "cpp", "h", "hpp"])
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
            .add_filter("すべてのファイル", &["*"])
            .add_filter("テキスト", &["txt", "text", "log"])
            .add_filter("Markdown", &["md", "markdown"])
            .add_filter("Rust", &["rs"])
            .add_filter("Python", &["py"])
            .add_filter("TypeScript / JavaScript", &["ts", "js", "tsx", "jsx"])
            .add_filter("HTML", &["html", "htm"])
            .add_filter("CSS", &["css"])
            .add_filter("JSON", &["json"])
            .add_filter("TOML", &["toml"])
            .add_filter("Kotlin", &["kt", "kts"])
            .add_filter("LaTeX", &["tex"])
            .add_filter("C / C++", &["c", "cpp", "h", "hpp"])
            .save_file(cb);
    })
    .map(|p| p.to_string_lossy().into_owned())
}

#[tauri::command(rename_all = "snake_case")]
fn save_session_state(app: tauri::AppHandle, state_json: String) -> Result<(), String> {
    let dir = app_config_dir(&app).ok_or_else(|| "設定ディレクトリがありません".to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let target = dir.join("session.json");
    let tmp = dir.join("session.json.tmp");
    std::fs::write(&tmp, state_json).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&target);
    std::fs::rename(tmp, target).map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "snake_case")]
fn read_session_state(app: tauri::AppHandle) -> Option<String> {
    let dir = app_config_dir(&app)?;
    std::fs::read_to_string(dir.join("session.json")).ok()
}

/// 文書を全部 1 つの文字列で webview へ渡さずに開きます。本体はネイティブ側の
/// ストアに置き、frontend は `read_lines` で窓を取り寄せます。開く時点でやるのは
/// 改行を数える 1 度の読み流しだけで、中身はメモリに置きません。
/// async なのは UI を待たせないため。
#[tauri::command]
async fn open_document(
    application: State<'_, Application>,
    path: String,
) -> Result<OpenedDocument, String> {
    application.open_document(path)
}

/// 指定した文字コードでファイルを開き直します。
#[tauri::command]
async fn reopen_document_encoding(
    application: State<'_, Application>,
    handle: u64,
    encoding: String,
) -> Result<ReopenedDocument, String> {
    application.reopen_document_encoding(handle, encoding)
}

/// 文書の文字コードを設定します（保存時に使われます）。
#[tauri::command]
async fn set_document_encoding(
    application: State<'_, Application>,
    handle: u64,
    encoding: String,
) -> Result<(), String> {
    application.set_document_encoding(handle, encoding)
}

/// 文書の改行コードを設定します（保存時に使われます）。
#[tauri::command(rename_all = "snake_case")]
async fn set_document_line_ending(
    application: State<'_, Application>,
    handle: u64,
    line_ending: String,
) -> Result<(), String> {
    application.set_document_line_ending(handle, line_ending)
}

/// 走査の完了を待ち、文書の行数を確定させる。
async fn wait_scanned(job: planetext_document::FinishDocumentJob) -> Result<usize, String> {
    loop {
        if let Some(line_count) = job.poll()? {
            return Ok(line_count);
        }
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    }
}

/// 走査の完了を待ってから確定した行数を返す。frontend は開いた直後に
/// これを 1 度呼び、返った行数で文書の長さを確定させる。
#[tauri::command]
async fn finish_document(
    application: State<'_, Application>,
    handle: u64,
) -> Result<usize, String> {
    wait_scanned(application.finish_document(handle)?).await
}

/// 新しい空の文書をストアに作ります。すべての文書の本体がネイティブ側にあります。
#[tauri::command]
fn create_document(application: State<'_, Application>) -> OpenedDocument {
    application.create_document()
}

/// 文書から行の範囲を返します。async なのは、同期コマンドはメインスレッドで
/// 走り、待たせた分だけ UI が止まるため（以下の文書コマンドも同じ）。
#[tauri::command]
async fn read_lines(
    application: State<'_, Application>,
    handle: u64,
    from: usize,
    count: usize,
) -> Result<Vec<String>, String> {
    application.read_lines(handle, from, count)
}

/// 行数走査を待たず、EOF基準で末尾の行を返します。
#[tauri::command]
async fn read_tail(
    application: State<'_, Application>,
    handle: u64,
    count: usize,
) -> Result<Vec<String>, String> {
    application.read_tail(handle, count)
}

/// 編集の到着: `from..to` の行を `lines` へ置き換えます。同じ `group` が続く間は
/// 元に戻す履歴の 1 ステップにつながります。新しい行数を返します。
#[tauri::command]
// 引数は frontend との受け渡しの形そのものなので、まとめると IPC の名前が変わる。
#[allow(clippy::too_many_arguments)]
async fn replace_lines(
    application: State<'_, Application>,
    handle: u64,
    from: usize,
    to: usize,
    lines: Vec<String>,
    group: u64,
    before: String,
    after: String,
) -> Result<usize, String> {
    application.replace_lines(handle, from, to, lines, group, before, after)
}

#[tauri::command]
async fn undo_lines(
    application: State<'_, Application>,
    handle: u64,
    redo: bool,
) -> Result<Option<RestoredLines>, String> {
    application.undo_lines(handle, redo)
}

/// 文書をストアからディスクへ直接書きます。全文は webview を通りません。
#[tauri::command]
async fn save_document(
    application: State<'_, Application>,
    handle: u64,
    path: String,
) -> Result<(), String> {
    application.save_document(handle, path)
}

/// 閉じられたタブの文書を手放します。
#[tauri::command]
fn close_document(application: State<'_, Application>, handle: u64) {
    application.close_document(handle);
}

/// 空のページをnative内で読み進め、最初の候補群までを1回のジョブで返す。
/// 文書は開始時に複製したピースと独立ファイルハンドルで読み、docsをロックしない。
#[tauri::command(rename_all = "snake_case")]
#[allow(clippy::too_many_arguments)]
async fn search_document(
    application: State<'_, Application>,
    handle: u64,
    query: String,
    regex: bool,
    case_sensitive: bool,
    needle: char,
    from: usize,
    end: usize,
    after_col: Option<usize>,
) -> Result<SearchPage, String> {
    let job = application.prepare_search(
        handle,
        query,
        regex,
        case_sensitive,
        needle,
        from,
        end,
        after_col,
    )?;
    tauri::async_runtime::spawn_blocking(move || job.run())
        .await
        .map_err(|e| format!("検索を続けられませんでした: {e}"))?
}

#[tauri::command]
fn cancel_search(application: State<'_, Application>, handle: u64) {
    application.cancel_search(handle);
}

/// 等間隔の行窓を標本にして、全文のおよその一致数を返します。
#[tauri::command(rename_all = "snake_case")]
async fn estimate_matches(
    application: State<'_, Application>,
    handle: u64,
    query: String,
    regex: bool,
    case_sensitive: bool,
) -> Result<usize, String> {
    application.estimate_matches(handle, query, regex, case_sensitive)
}

/// 範囲内で `needle` を含む行。frontend が読み替えの必要な行を探すのに使います。
/// 何の文字に意味があるか（保存形式）はこちらでは知りません。
#[tauri::command]
async fn lines_containing(
    application: State<'_, Application>,
    handle: u64,
    from: usize,
    to: usize,
    needle: char,
) -> Result<Vec<usize>, String> {
    let started = Instant::now();
    let found = application.lines_containing(handle, from, to, needle)?;
    store_log("lines_containing", started);
    Ok(found)
}

/// 選択された範囲を組み立てて、システムのクリップボードへ置きます。
/// 全文が webview を通らないので、大きな選択のコピーも一息で済みます。
/// 端の行の切り出しと、読み替えの必要な行は frontend が渡してきます。
#[tauri::command]
async fn copy_range(
    application: State<'_, Application>,
    handle: u64,
    from: usize,
    first: Option<String>,
    to: usize,
    last: Option<String>,
    overrides: Vec<(usize, String)>,
) -> Result<(), String> {
    let started = Instant::now();
    let text = application.copy_range(handle, from, first, to, last, overrides)?;
    store_log("copy assemble", started);
    let started = Instant::now();
    let result = platform::set_clipboard_text(text);
    store_log("copy clipboard", started);
    result
}

/// `PLANETEXT_STORE_LOG` が設定されていれば、文書ストアの重い操作の時間を出す。
/// どこで時間が消えているかを、実際の操作で測るための窓。
fn store_log(what: &str, started: Instant) {
    if std::env::var_os("PLANETEXT_STORE_LOG").is_some() {
        eprintln!("store: {what}: {} ms", started.elapsed().as_millis());
    }
}

fn app_config_dir(app: &tauri::AppHandle) -> Option<PathBuf> {
    app.path().app_config_dir().ok()
}

#[tauri::command]
fn read_settings(app: tauri::AppHandle, application: State<'_, Application>) -> String {
    application.read_settings(app_config_dir(&app))
}

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.set_zoom(1.0);
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn toggle_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let is_visible = window.is_visible().unwrap_or(false);
        let is_minimized = window.is_minimized().unwrap_or(false);
        let is_focused = window.is_focused().unwrap_or(false);
        if is_visible && !is_minimized && is_focused {
            let _ = window.hide();
        } else {
            let _ = window.show();
            let _ = window.unminimize();
            let _ = window.set_focus();
        }
    }
}

fn dispatch_gui_event(app: &tauri::AppHandle, event: GuiEvent) {
    match app.state::<Application>().handle_gui_event(event) {
        GuiAction::ShowWindow => show_main_window(app),
        GuiAction::HideWindow => {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.hide();
            }
        }
        GuiAction::ToggleWindow => toggle_main_window(app),
        GuiAction::Exit => app.exit(0),
        GuiAction::ConfirmExit => {
            if let Some(window) = app.get_webview_window("main") {
                show_main_window(app);
                confirm_discard_on_close(&window);
            } else {
                app.exit(0);
            }
        }
    }
}

fn sync_global_shortcut(app: &tauri::AppHandle, settings: GlobalShortcutSettings) {
    debug_log(&format!(
        "[SHORTCUT] Checking shortcut: enabled={}, key={}",
        settings.enabled, settings.key
    ));
    let gs = app.global_shortcut();

    if settings.enabled {
        let mut target_keys = vec![settings.key.as_str()];
        if settings.key == "Ctrl+Shift+M" {
            target_keys.push("Ctrl+Shift+M");
        }
        for key_str in target_keys {
            if let Ok(shortcut) = key_str.parse::<Shortcut>() {
                if !gs.is_registered(shortcut) {
                    match gs.register(shortcut) {
                        Ok(_) => {
                            debug_log(&format!("[SHORTCUT] Successfully registered {key_str}"));
                            break;
                        }
                        Err(e) => {
                            debug_log(&format!(
                                "[SHORTCUT ERROR] Failed to register {key_str}: {e}"
                            ));
                        }
                    }
                } else {
                    debug_log(&format!("[SHORTCUT] Already registered {key_str}"));
                    break;
                }
            }
        }
    } else if let Ok(shortcut) = settings.key.parse::<Shortcut>() {
        if gs.is_registered(shortcut) {
            let _ = gs.unregister(shortcut);
            debug_log(&format!("[SHORTCUT] Unregistered {}", settings.key));
        }
    }
}

fn setup_tray(app: &tauri::AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    debug_log("[TRAY] Setting up tray icon...");
    let open_item = MenuItemBuilder::with_id("open", "Planetext を開く").build(app)?;
    let quit_item = MenuItemBuilder::with_id("quit", "終了").build(app)?;
    let menu = MenuBuilder::new(app)
        .item(&open_item)
        .separator()
        .item(&quit_item)
        .build()?;

    let icon = app
        .default_window_icon()
        .cloned()
        .unwrap_or_else(|| tauri::include_image!("icons/32x32.png"));

    let tray = TrayIconBuilder::new()
        .icon(icon)
        .icon_as_template(false)
        .menu(&menu)
        .show_menu_on_left_click(true)
        .tooltip("Planetext")
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => dispatch_gui_event(app, GuiEvent::TraySelected(TrayAction::Open)),
            "quit" => dispatch_gui_event(app, GuiEvent::TraySelected(TrayAction::Quit)),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| match event {
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            }
            | TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } => {
                dispatch_gui_event(tray.app_handle(), GuiEvent::TraySelected(TrayAction::Open));
            }
            _ => {}
        })
        .build(app)?;

    if let Some(state) = app.try_state::<AppState>() {
        state.tray.lock().unwrap().replace(tray);
    }
    debug_log("[TRAY] Tray icon initialized and stored in AppState successfully.");

    Ok(())
}

#[tauri::command]
fn write_settings(
    app: tauri::AppHandle,
    application: State<'_, Application>,
    contents: String,
) -> Result<(), String> {
    let write = application.prepare_settings_write(app_config_dir(&app), contents)?;
    sync_global_shortcut(&app, global_shortcut_settings(write.contents()));
    write.write()
}

#[tauri::command]
async fn save_draft(
    app: tauri::AppHandle,
    application: State<'_, Application>,
    handle: u64,
    id: String,
    path: Option<String>,
) -> Result<(), String> {
    application.save_draft(app_config_dir(&app), handle, id, path)
}

#[tauri::command]
fn file_size(
    app: tauri::AppHandle,
    application: State<'_, Application>,
    path: Option<String>,
    id: String,
) -> Option<u64> {
    application.file_size(app_config_dir(&app), path, id)
}

#[tauri::command]
fn remove_draft(app: tauri::AppHandle, application: State<'_, Application>, id: String) {
    application.remove_draft(app_config_dir(&app), id);
}

#[tauri::command]
fn read_drafts(app: tauri::AppHandle, application: State<'_, Application>) -> Vec<Draft> {
    application.read_drafts(app_config_dir(&app))
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
fn set_dirty(application: State<'_, Application>, dirty: bool) {
    application.set_dirty(dirty);
}

/// `PLANETEXT_STARTUP_LOG` が設定されている場合、プロセスの開始から最初のフロントエンド ペイントまでの時間を報告します。起動コストを監視するために使用されます。
#[tauri::command]
fn frontend_ready(app: tauri::AppHandle, state: State<'_, AppState>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.set_zoom(1.0);
    }
    if std::env::var_os("PLANETEXT_STARTUP_LOG").is_some() {
        eprintln!(
            "planetext startup: {} ms",
            state.started.elapsed().as_millis()
        );
    }
}

#[tauri::command]
fn open_external_url(url: String) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        std::process::Command::new("cmd")
            .args(["/c", "start", "", &url])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = std::process::Command::new("xdg-open").arg(&url).spawn();
        Ok(())
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

fn confirm_discard_on_close(window: &tauri::WebviewWindow) {
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
                let application = target.state::<Application>();
                application.clear_drafts(app_config_dir(target.app_handle()));
                application.clear_dirty();
                let _ = target.destroy();
            }
        });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri::plugin::Builder::<tauri::Wry>::new("planetext-host")
                .js_init_script(
                    "window.__PLANETEXT_HOST__ = { core: { invoke: (command, args) => window.__TAURI__.core.invoke(command, args) }, event: { listen: (name, handler) => window.__TAURI__.event.listen(name, handler) } };",
                )
                .build(),
        )
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            dispatch_gui_event(app, GuiEvent::SecondInstance);
        }))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    debug_log(&format!(
                        "[SHORTCUT EVENT] Shortcut {shortcut:?} triggered, state={:?}",
                        event.state()
                    ));
                    if event.state() == tauri_plugin_global_shortcut::ShortcutState::Pressed {
                        dispatch_gui_event(app, GuiEvent::GlobalShortcut(format!("{shortcut:?}")));
                    }
                })
                .build(),
        )
        .manage(Application::default())
        .manage(AppState {
            started: Instant::now(),
            tray: Mutex::new(None),
        })
        .setup(|app| {
            debug_log("[SETUP] Starting setup hook...");
            restore_window_size(app.handle());
            if let Err(e) = menu::install(app.handle()) {
                debug_log(&format!("[SETUP ERROR] menu::install failed: {e}"));
            }
            if let Err(e) = setup_tray(app.handle()) {
                debug_log(&format!("[SETUP ERROR] setup_tray failed: {e}"));
            }
            let settings = app
                .state::<Application>()
                .read_settings(app_config_dir(app.handle()));
            debug_log(&format!(
                "[SETUP] Settings loaded: length={}",
                settings.len()
            ));
            sync_global_shortcut(app.handle(), global_shortcut_settings(&settings));
            show_main_window(app.handle());
            debug_log("[SETUP] Setup hook completed and main window shown.");
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                save_window_size(window);
                api.prevent_close();
                dispatch_gui_event(window.app_handle(), GuiEvent::CloseRequested);
            }
        })
        .invoke_handler(tauri::generate_handler![
            pick_open_path,
            pick_save_path,
            open_document,
            finish_document,
            create_document,
            read_lines,
            read_tail,
            replace_lines,
            undo_lines,
            save_document,
            close_document,
            lines_containing,
            copy_range,
            search_document,
            cancel_search,
            estimate_matches,
            set_dirty,
            frontend_ready,
            confirm_discard,
            read_settings,
            write_settings,
            save_draft,
            remove_draft,
            read_drafts,
            save_session_state,
            read_session_state,
            file_size,
            reopen_document_encoding,
            set_document_encoding,
            set_document_line_ending,
            open_external_url,
            menu::sync_view_menu
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
