mod menu;
mod store;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Manager, State, WindowEvent};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};

const GLOBAL_SHORTCUT: &str = "Ctrl+Alt+M";

struct AppState {
    dirty: Mutex<bool>,
    started: Instant,
    /// 開いている文書の本体。webview は行の窓だけを取り寄せ、編集は
    /// 行範囲の置き換えとして届く。タブが閉じられると手放す。
    docs: Mutex<HashMap<u64, store::Document>>,
    next_document: Mutex<u64>,
}

impl AppState {
    fn adopt(&self, doc: store::Document) -> OpenedDocument {
        let opened = OpenedDocument {
            handle: {
                let mut next = self.next_document.lock().unwrap();
                *next += 1;
                *next
            },
            line_count: doc.line_count(),
            bytes: doc.bytes(),
        };
        self.docs.lock().unwrap().insert(opened.handle, doc);
        opened
    }
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

/// 開き方の答え: 文書の取っ手と、行数と大きさ。
#[derive(serde::Serialize)]
struct OpenedDocument {
    handle: u64,
    line_count: usize,
    bytes: usize,
}

/// 文書を全部 1 つの文字列で webview へ渡さずに開きます。本体はネイティブ側の
/// ストアに置き、frontend は `read_lines` で窓を取り寄せます。開く時点でやるのは
/// 改行を数える 1 度の読み流しだけで、中身はメモリに置きません。
/// async なのは UI を待たせないため。
#[tauri::command]
async fn open_document(state: State<'_, AppState>, path: String) -> Result<OpenedDocument, String> {
    Ok(state.adopt(store::Document::open(&path)?))
}

/// 新しい空の文書をストアに作ります。すべての文書の本体がネイティブ側にあります。
#[tauri::command]
fn create_document(state: State<'_, AppState>) -> OpenedDocument {
    state.adopt(store::Document::empty())
}

/// 文書から行の範囲を返します。async なのは、同期コマンドはメインスレッドで
/// 走り、待たせた分だけ UI が止まるため（以下の文書コマンドも同じ）。
#[tauri::command]
async fn read_lines(
    state: State<'_, AppState>,
    handle: u64,
    from: usize,
    count: usize,
) -> Result<Vec<String>, String> {
    let mut docs = state.docs.lock().unwrap();
    let Some(doc) = docs.get_mut(&handle) else {
        return Err("文書はもう閉じられています".to_string());
    };
    doc.read(from, count)
}

/// 編集の到着: `from..to` の行を `lines` へ置き換えます。同じ `group` が続く間は
/// 元に戻す履歴の 1 ステップにつながります。新しい行数を返します。
#[tauri::command]
async fn replace_lines(
    state: State<'_, AppState>,
    handle: u64,
    from: usize,
    to: usize,
    lines: Vec<String>,
    group: u64,
    before: String,
    after: String,
) -> Result<usize, String> {
    let mut docs = state.docs.lock().unwrap();
    let Some(doc) = docs.get_mut(&handle) else {
        return Err("文書はもう閉じられています".to_string());
    };
    doc.replace(from, to, lines, group, &before, &after)
}

/// 元に戻す・やり直すの結果。`state` は frontend が預けた控えそのもの。
#[derive(serde::Serialize)]
struct RestoredLines {
    state: String,
    touched_from: usize,
    line_count: usize,
}

#[tauri::command]
async fn undo_lines(
    state: State<'_, AppState>,
    handle: u64,
    redo: bool,
) -> Result<Option<RestoredLines>, String> {
    let mut docs = state.docs.lock().unwrap();
    let Some(doc) = docs.get_mut(&handle) else {
        return Ok(None);
    };
    Ok(
        (if redo { doc.redo() } else { doc.undo() })?.map(|restored| RestoredLines {
            state: restored.state,
            touched_from: restored.touched_from,
            line_count: restored.line_count,
        }),
    )
}

/// 文書をストアからディスクへ直接書きます。全文は webview を通りません。
#[tauri::command]
async fn save_document(
    state: State<'_, AppState>,
    handle: u64,
    path: String,
) -> Result<(), String> {
    let mut docs = state.docs.lock().unwrap();
    let Some(doc) = docs.get_mut(&handle) else {
        return Err("文書はもう閉じられています".to_string());
    };
    doc.save(&path)
}

/// 閉じられたタブの文書を手放します。
#[tauri::command]
fn close_document(state: State<'_, AppState>, handle: u64) {
    state.docs.lock().unwrap().remove(&handle);
}

/// 検索の 1 ページ分の走査。素の行の一致と、読み替えの要る行が行の順で返り、
/// `scanned_to` から続きを頼めます。パターンの意味は regex クレートのもの。
#[derive(serde::Serialize)]
struct ScanPage {
    hits: Vec<store::ScanHit>,
    scanned_to: usize,
}

// 複数語の引数（case_sensitive）を snake_case のまま受け取る。既定の camelCase
// 期待のままだと、frontend の引数が見つからず呼び出しが失敗する。
#[tauri::command(rename_all = "snake_case")]
#[allow(clippy::too_many_arguments)]
async fn search_lines(
    state: State<'_, AppState>,
    handle: u64,
    query: String,
    regex: bool,
    case_sensitive: bool,
    needle: char,
    from: usize,
    count: usize,
) -> Result<ScanPage, String> {
    let pattern = if regex {
        query
    } else {
        regex::escape(&query)
    };
    let pattern = regex::RegexBuilder::new(&pattern)
        .case_insensitive(!case_sensitive)
        .build()
        .map_err(|e| format!("正規表現を読めませんでした: {e}"))?;
    let mut docs = state.docs.lock().unwrap();
    let Some(doc) = docs.get_mut(&handle) else {
        return Err("文書はもう閉じられています".to_string());
    };
    let (hits, scanned_to) = doc.scan(&pattern, needle, from, count, 64)?;
    Ok(ScanPage { hits, scanned_to })
}

/// 範囲内で `needle` を含む行。frontend が読み替えの必要な行を探すのに使います。
/// 何の文字に意味があるか（保存形式）はこちらでは知りません。
#[tauri::command]
async fn lines_containing(
    state: State<'_, AppState>,
    handle: u64,
    from: usize,
    to: usize,
    needle: char,
) -> Result<Vec<usize>, String> {
    let started = Instant::now();
    let mut docs = state.docs.lock().unwrap();
    let Some(doc) = docs.get_mut(&handle) else {
        return Err("文書はもう閉じられています".to_string());
    };
    let found = doc.lines_containing(from, to, needle)?;
    store_log("lines_containing", started);
    Ok(found)
}

/// 選択された範囲を組み立てて、システムのクリップボードへ置きます。
/// 全文が webview を通らないので、大きな選択のコピーも一息で済みます。
/// 端の行の切り出しと、読み替えの必要な行は frontend が渡してきます。
#[tauri::command]
async fn copy_range(
    state: State<'_, AppState>,
    handle: u64,
    from: usize,
    first: Option<String>,
    to: usize,
    last: Option<String>,
    overrides: Vec<(usize, String)>,
) -> Result<(), String> {
    let started = Instant::now();
    let text = {
        let mut docs = state.docs.lock().unwrap();
        let Some(doc) = docs.get_mut(&handle) else {
            return Err("文書はもう閉じられています".to_string());
        };
        doc.assemble(from, first, to, last, &overrides.into_iter().collect())?
    };
    store_log("copy assemble", started);
    let started = Instant::now();
    let result = arboard::Clipboard::new()
        .and_then(|mut clipboard| clipboard.set_text(text))
        .map_err(|e| format!("コピーできませんでした: {e}"));
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

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn toggle_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let is_visible = window.is_visible().unwrap_or(false);
        let is_focused = window.is_focused().unwrap_or(false);
        if is_visible && is_focused {
            let _ = window.hide();
        } else {
            let _ = window.show();
            let _ = window.unminimize();
            let _ = window.set_focus();
        }
    }
}

fn sync_global_shortcut(app: &tauri::AppHandle, settings_text: &str) {
    let enabled = !settings_text.lines().any(|line| {
        let Some((k, v)) = line.split_once('=') else {
            return false;
        };
        k.trim() == "global_shortcut" && v.trim() == "false"
    });

    let Ok(shortcut) = GLOBAL_SHORTCUT.parse::<Shortcut>() else {
        return;
    };
    let gs = app.global_shortcut();
    if enabled {
        if !gs.is_registered(shortcut) {
            let _ = gs.register(shortcut);
        }
    } else {
        if gs.is_registered(shortcut) {
            let _ = gs.unregister(shortcut);
        }
    }
}

fn setup_tray(app: &tauri::AppHandle) -> Result<(), Box<dyn std::error::Error>> {
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
        .unwrap_or_else(|| tauri::include_image!("icons/icon.png"));

    let _tray = TrayIconBuilder::with_id("tray")
        .icon(icon)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("Planetext")
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => {
                show_main_window(app);
            }
            "quit" => {
                if is_dirty(app) {
                    if let Some(window) = app.get_webview_window("main") {
                        show_main_window(app);
                        confirm_discard_on_close(&window);
                        return;
                    }
                }
                app.exit(0);
            }
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
                show_main_window(tray.app_handle());
            }
            _ => {}
        })
        .build(app)?;

    Ok(())
}

#[tauri::command]
fn write_settings(app: tauri::AppHandle, contents: String) -> Result<(), String> {
    let Some(path) = settings_path(&app) else {
        return Err("設定の保存先がありません".to_string());
    };
    sync_global_shortcut(&app, &contents);
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

/// 文書の本体から下書きを書きます。最初の行はドキュメントのパス、続きが本文。
#[tauri::command]
async fn save_draft(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    handle: u64,
    id: String,
    path: Option<String>,
) -> Result<(), String> {
    let Some(dir) = drafts_dir(&app) else {
        return Err("下書きの保存先がありません".to_string());
    };
    let mut docs = state.docs.lock().unwrap();
    let Some(doc) = docs.get_mut(&handle) else {
        return Err("文書はもう閉じられています".to_string());
    };
    let file = std::fs::File::create(dir.join(draft_name(&id)))
        .map_err(|e| format!("下書きを保存できませんでした: {e}"))?;
    let mut out = std::io::BufWriter::new(file);
    use std::io::Write;
    writeln!(out, "{}", path.unwrap_or_default())
        .map_err(|e| format!("下書きを保存できませんでした: {e}"))?;
    doc.write_to(&mut out)
        .map_err(|e| format!("下書きを保存できませんでした: {e}"))
}

/// 保存済みファイル、未保存なら下書きファイルのサイズを返します。
/// どちらも読めないときは `None` を返します（呼び出し側が不明として扱います）。
#[tauri::command]
fn file_size(app: tauri::AppHandle, path: Option<String>, id: String) -> Option<u64> {
    let target = match path {
        Some(p) => PathBuf::from(p),
        None => drafts_dir(&app)?.join(draft_name(&id)),
    };
    std::fs::metadata(&target).map(|m| m.len()).ok()
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

fn is_dirty(app: &tauri::AppHandle) -> bool {
    app.state::<AppState>()
        .dirty
        .lock()
        .map(|dirty| *dirty)
        .unwrap_or(false)
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
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            show_main_window(app);
        }))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state() == tauri_plugin_global_shortcut::ShortcutState::Pressed {
                        toggle_main_window(app);
                    }
                })
                .build(),
        )
        .manage(AppState {
            dirty: Mutex::new(false),
            started: Instant::now(),
            docs: Mutex::new(HashMap::new()),
            next_document: Mutex::new(0),
        })
        .setup(|app| {
            restore_window_size(app.handle());
            menu::install(app.handle())?;
            setup_tray(app.handle())?;
            let settings = read_settings(app.handle().clone());
            sync_global_shortcut(app.handle(), &settings);
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                save_window_size(window);
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            pick_open_path,
            pick_save_path,
            open_document,
            create_document,
            read_lines,
            replace_lines,
            undo_lines,
            save_document,
            close_document,
            lines_containing,
            copy_range,
            search_lines,
            set_dirty,
            frontend_ready,
            confirm_discard,
            read_settings,
            write_settings,
            save_draft,
            remove_draft,
            read_drafts,
            file_size,
            menu::sync_view_menu
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
