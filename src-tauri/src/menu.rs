//! オペレーティング システム独自のメニューとしてのアプリケーションのメニュー: Windows および Linux ではウィンドウ内のメニュー バー、macOS では画面上部のメニュー バー。
//!
//! メニュー項目の意味は、同じことを行うキーの隣のフロントエンドに存在するため、項目を選択すると、その名前が「menu」イベントとして送信されるだけです。切り取り、コピー、貼り付けはシステム独自の項目で、キーと同じようにテキストにアクセスできます。

#[cfg(target_os = "macos")]
use tauri::menu::WINDOW_SUBMENU_ID;
use tauri::menu::{
    AboutMetadata, CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu, HELP_SUBMENU_ID,
};
use tauri::{AppHandle, Emitter, Manager, Runtime, State, Wry};

/// 何かがオンになっているかどうかを示す項目は、フロントエンドが設定内容をメニューに伝えることができるように保持されます。
pub struct Checks<R: Runtime> {
    wrap: CheckMenuItem<R>,
    line_numbers: CheckMenuItem<R>,
    show_whitespace: CheckMenuItem<R>,
    split: CheckMenuItem<R>,
}

/// Tells the check marks in 表示 what is currently on. Called by the frontend
/// once the settings are read and whenever one of them changes, so that the
/// menu and the screen never disagree.
#[tauri::command]
pub fn sync_view_menu(
    state: State<'_, Checks<Wry>>,
    wrap: bool,
    line_numbers: bool,
    show_whitespace: bool,
    split: bool,
) {
    state.wrap.set_checked(wrap).ok();
    state.line_numbers.set_checked(line_numbers).ok();
    state.show_whitespace.set_checked(show_whitespace).ok();
    state.split.set_checked(split).ok();
}

/// メニュー バーを作成し、選択された内容をフロントエンドに送信し始めます。
pub fn install(app: &AppHandle) -> tauri::Result<()> {
    let item = |id: &str, text: &str, accelerator: Option<&str>| {
        #[cfg(not(target_os = "macos"))]
        {
            let label = match accelerator {
                Some(acc) => {
                    let key = acc.replace("CmdOrCtrl+", "Ctrl+").replace("Cmd+", "Ctrl+");
                    format!("{text}\t{key}")
                }
                None => text.to_string(),
            };
            MenuItem::with_id(app, id, &label, true, None::<&str>)
        }
        #[cfg(target_os = "macos")]
        {
            MenuItem::with_id(app, id, text, true, accelerator)
        }
    };
    let check = |id: &str, text: &str, accelerator: Option<&str>| {
        #[cfg(not(target_os = "macos"))]
        {
            let label = match accelerator {
                Some(acc) => {
                    let key = acc.replace("CmdOrCtrl+", "Ctrl+").replace("Cmd+", "Ctrl+");
                    format!("{text}\t{key}")
                }
                None => text.to_string(),
            };
            CheckMenuItem::with_id(app, id, &label, true, false, None::<&str>)
        }
        #[cfg(target_os = "macos")]
        {
            CheckMenuItem::with_id(app, id, text, true, false, accelerator)
        }
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
            &item("print", "印刷…", Some("CmdOrCtrl+P"))?,
            &separator()?,
            &item("close_tab", "タブを閉じる", Some("CmdOrCtrl+W"))?,
        ],
    )?;
    // macOS では、設定と終了はアプリケーション独自のメニューに属します。
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
            &item("insert_structure", "構造パレット", Some("CmdOrCtrl+M"))?,
        ],
    )?;

    let zoom_in = item("zoom_in", "拡大", Some("CmdOrCtrl+="))?;
    let zoom_out = item("zoom_out", "縮小", Some("CmdOrCtrl+-"))?;
    let zoom_reset = item("zoom_reset", "実際のサイズ", Some("CmdOrCtrl+0"))?;

    let checks = Checks {
        wrap: check("wrap", "折り返す", None)?,
        line_numbers: check("line_numbers", "行番号を表示する", None)?,
        show_whitespace: check("show_whitespace", "空白文字を表示する", Some("Alt+Z"))?,
        split: check("split", "左右に分割", Some("CmdOrCtrl+\\"))?,
    };
    let view = Submenu::with_items(
        app,
        "表示",
        true,
        &[
            &zoom_in,
            &zoom_out,
            &zoom_reset,
            &separator()?,
            &checks.wrap,
            &checks.line_numbers,
            &checks.show_whitespace,
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

/// Windows と Linux では、Ctrl+Y でやり直します。 ⇧⌘Z.
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

/// を備えた macOS では、終了はウィンドウ自体を閉じることによって行われるため、保存されていない作業内容が引き続き確認されます。それ以外はすべてフロントエンドが実行します。
fn chosen(app: &AppHandle, id: &str) {
    if id == "quit" {
        if let Some(window) = app.get_webview_window("main") {
            window.close().ok();
        }
        return;
    }
    app.emit("menu", id).ok();
}
