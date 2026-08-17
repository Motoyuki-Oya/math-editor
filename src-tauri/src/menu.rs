//! The application's menus, as the operating system's own menus: a menu bar in
//! the window on Windows and Linux, and the menu bar at the top of the screen
//! on macOS.
//!
//! What a menu item means lives in the frontend, next to the keys that do the
//! same things, so choosing an item only sends its name across as a `menu`
//! event. Cutting, copying and pasting are the system's own items, which reach
//! the text the same way the keys do.

#[cfg(target_os = "macos")]
use tauri::menu::WINDOW_SUBMENU_ID;
use tauri::menu::{
    AboutMetadata, CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu, HELP_SUBMENU_ID,
};
use tauri::{AppHandle, Emitter, Manager, Runtime, State, Wry};

/// The items that show whether something is on, kept so that the frontend can
/// tell the menu what the settings say.
pub struct Checks<R: Runtime> {
    wrap: CheckMenuItem<R>,
    line_numbers: CheckMenuItem<R>,
    split: CheckMenuItem<R>,
}

/// Tells the check marks in 表示 what is currently on. Called by the frontend
/// once the settings are read and whenever one of them changes, so that the
/// menu and the screen never disagree.
#[tauri::command]
pub fn sync_view_menu(state: State<'_, Checks<Wry>>, wrap: bool, line_numbers: bool, split: bool) {
    state.wrap.set_checked(wrap).ok();
    state.line_numbers.set_checked(line_numbers).ok();
    state.split.set_checked(split).ok();
}

/// Builds the menu bar and starts sending what is chosen to the frontend.
pub fn install(app: &AppHandle) -> tauri::Result<()> {
    let item = |id: &str, text: &str, accelerator: Option<&str>| {
        MenuItem::with_id(app, id, text, true, accelerator)
    };
    let check = |id: &str, text: &str, accelerator: Option<&str>| {
        CheckMenuItem::with_id(app, id, text, true, false, accelerator)
    };
    let separator = || PredefinedMenuItem::separator(app);

    let preferences = item("preferences", "設定…", Some("CmdOrCtrl+,"))?;
    let about = PredefinedMenuItem::about(
        app,
        Some("Planetext について"),
        Some(AboutMetadata {
            name: Some("Planetext".into()),
            version: Some(env!("CARGO_PKG_VERSION").into()),
            comments: Some("二次元の形をふつうの文字として書けるテキストエディタ".into()),
            ..Default::default()
        }),
    )?;

    let file = Submenu::with_items(
        app,
        "ファイル",
        true,
        &[
            &item("new", "新規", Some("CmdOrCtrl+N"))?,
            &item("open", "開く…", Some("CmdOrCtrl+O"))?,
            &separator()?,
            &item("save", "保存", Some("CmdOrCtrl+S"))?,
            &item("save_as", "名前を付けて保存…", Some("CmdOrCtrl+Shift+S"))?,
            &separator()?,
            &item("close_tab", "タブを閉じる", Some("CmdOrCtrl+W"))?,
        ],
    )?;
    // On macOS the settings and quitting belong in the application's own menu.
    #[cfg(not(target_os = "macos"))]
    {
        file.append_items(&[
            &separator()?,
            &preferences,
            &separator()?,
            &item("quit", "終了", Some("CmdOrCtrl+Q"))?,
        ])?;
    }

    let edit = Submenu::with_items(
        app,
        "編集",
        true,
        &[
            &item("undo", "元に戻す", Some("CmdOrCtrl+Z"))?,
            &item("redo", "やり直す", Some(redo_accelerator()))?,
            &separator()?,
            &PredefinedMenuItem::cut(app, Some("切り取り"))?,
            &PredefinedMenuItem::copy(app, Some("コピー"))?,
            &PredefinedMenuItem::paste(app, Some("貼り付け"))?,
            &item("select_all", "すべて選択", Some("CmdOrCtrl+A"))?,
            &separator()?,
            &item("find", "検索…", Some("CmdOrCtrl+F"))?,
            &item("replace", "置換…", Some("CmdOrCtrl+R"))?,
            &separator()?,
            &item("insert_math", "数式を入れる", Some("CmdOrCtrl+M"))?,
        ],
    )?;

    let checks = Checks {
        wrap: check("wrap", "折り返す", None)?,
        line_numbers: check("line_numbers", "行番号を表示する", None)?,
        split: check("split", "左右に分割", Some("CmdOrCtrl+\\"))?,
    };
    let view = Submenu::with_items(
        app,
        "表示",
        true,
        &[
            &checks.wrap,
            &checks.line_numbers,
            &separator()?,
            &checks.split,
        ],
    )?;

    let help = Submenu::with_id_and_items(app, HELP_SUBMENU_ID, "ヘルプ", true, &[&about])?;

    #[cfg(target_os = "macos")]
    let application = Submenu::with_items(
        app,
        "Planetext",
        true,
        &[
            &about,
            &separator()?,
            &preferences,
            &separator()?,
            &PredefinedMenuItem::hide(app, Some("Planetext を隠す"))?,
            &PredefinedMenuItem::hide_others(app, Some("ほかを隠す"))?,
            &PredefinedMenuItem::show_all(app, Some("すべてを表示"))?,
            &separator()?,
            &item("quit", "Planetext を終了", Some("Cmd+Q"))?,
        ],
    )?;
    #[cfg(target_os = "macos")]
    let window = Submenu::with_id_and_items(
        app,
        WINDOW_SUBMENU_ID,
        "ウインドウ",
        true,
        &[
            &PredefinedMenuItem::minimize(app, Some("しまう"))?,
            &PredefinedMenuItem::maximize(app, Some("拡大／縮小"))?,
            &separator()?,
            &PredefinedMenuItem::fullscreen(app, Some("フルスクリーンにする"))?,
        ],
    )?;
    #[cfg(target_os = "macos")]
    let menus: Vec<&dyn tauri::menu::IsMenuItem<Wry>> =
        vec![&application, &file, &edit, &view, &window, &help];
    #[cfg(not(target_os = "macos"))]
    let menus: Vec<&dyn tauri::menu::IsMenuItem<Wry>> = vec![&file, &edit, &view, &help];

    let menu = Menu::with_items(app, &menus)?;
    app.set_menu(menu)?;
    app.manage(checks);

    app.on_menu_event(|app, event| chosen(app, event.id().as_ref()));
    Ok(())
}

/// Windows and Linux redo with Ctrl+Y; macOS with ⇧⌘Z.
fn redo_accelerator() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "Cmd+Shift+Z"
    }
    #[cfg(not(target_os = "macos"))]
    {
        "Ctrl+Y"
    }
}

/// Quitting goes through the window's own closing, so that unsaved work is
/// still asked about. Everything else is the frontend's to carry out.
fn chosen(app: &AppHandle, id: &str) {
    if id == "quit" {
        if let Some(window) = app.get_webview_window("main") {
            window.close().ok();
        }
        return;
    }
    app.emit("menu", id).ok();
}
