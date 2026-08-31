//! ユーザーの設定: すべてのデフォルトに対して 1 つの場所、およびそれらをオーバーライドするファイル。
//!
//! 保存された表記法は設定ではありません。これはファイル形式そのものであり、これを構成可能にすると、他の場所ではファイルが読み取れなくなります。ビューが使用する DOM クラスも設定ではありません。

use std::cell::RefCell;

use wasm_bindgen::JsCast;

/// ユーザーが変更できるすべてのもの。設定ウィンドウに表示されるのはサブセットです。ユーザーが触れる必要のない値はファイル内にのみ残ります。
#[derive(Clone, PartialEq)]
pub struct Settings {
    /// エディター内のテキストのサイズ (ピクセル単位)。
    pub font_size: f64,
    /// テキストのフォント。空は組み込みのデフォルトを意味します。
    pub font_family: String,
    /// キャレットが点滅するかどうか。
    pub caret_blink: bool,
    /// ウィンドウに対して長すぎる行が横からはみ出さずに下に引き継がれるかどうか。
    pub wrap: bool,
    /// 各行にその番号が表示されるかどうか。
    pub line_numbers: bool,
    /// 整列された列間のギャップ (ピクセル単位)。ファイルのみ。
    pub column_gap: f64,
    /// 元に戻す履歴が保持されるステップ数。ファイルのみ。
    pub history_limit: usize,
    /// グローバルショートカット (Ctrl+Alt+M) でアプリを呼び出せるかどうか。
    pub global_shortcut: bool,
    /// 半角スペース、全角スペース、タブなどの空白文字を可視化するかどうか。
    pub show_whitespace: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            font_size: 14.0,
            font_family: String::new(),
            caret_blink: true,
            wrap: true,
            line_numbers: false,
            column_gap: 18.0,
            history_limit: 500,
            global_shortcut: true,
            show_whitespace: false,
        }
    }
}

thread_local! {
    static CURRENT: RefCell<Settings> = RefCell::new(Settings::default());
}

/// 現在有効な設定。
pub fn current() -> Settings {
    CURRENT.with(|current| current.borrow().clone())
}

pub fn column_gap() -> f64 {
    CURRENT.with(|current| current.borrow().column_gap)
}

pub fn line_numbers() -> bool {
    CURRENT.with(|current| current.borrow().line_numbers)
}

pub fn wrap() -> bool {
    CURRENT.with(|current| current.borrow().wrap)
}

#[allow(dead_code)]
pub fn show_whitespace() -> bool {
    CURRENT.with(|current| current.borrow().show_whitespace)
}

/// フォントサイズを拡大（ズームイン）します。
#[allow(dead_code)]
pub fn zoom_in() {
    let mut s = current();
    if s.font_size < 48.0 {
        s.font_size = (s.font_size + 1.0).min(48.0);
        apply(s);
        crate::editor::redraw_all();
    }
}

/// フォントサイズを縮小（ズームアウト）します。
#[allow(dead_code)]
pub fn zoom_out() {
    let mut s = current();
    if s.font_size > 9.0 {
        s.font_size = (s.font_size - 1.0).max(9.0);
        apply(s);
        crate::editor::redraw_all();
    }
}

/// フォントサイズを標準（14px）にリセットします。
#[allow(dead_code)]
pub fn zoom_reset() {
    let mut s = current();
    s.font_size = 14.0;
    apply(s);
    crate::editor::redraw_all();
}

/// 空白文字の可視化トグルを切り替えます。
pub fn toggle_whitespace() {
    let mut s = current();
    s.show_whitespace = !s.show_whitespace;
    apply(s);
    crate::editor::redraw_all();
}

/// 「設定」を有効にし、視覚的な設定を画面に表示します。
pub fn apply(settings: Settings) {
    show(&settings);
    CURRENT.with(|current| *current.borrow_mut() = settings);
}

/// 視覚的な設定はドキュメント ルートの CSS 変数として画面に到達するため、スタイルシートは見た目を決定する唯一の場所のままです。
fn show(settings: &Settings) {
    let Some(root) = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.document_element())
    else {
        return;
    };
    let root: web_sys::HtmlElement = match root.dyn_into() {
        Ok(root) => root,
        Err(_) => return,
    };
    let style = root.style();
    style
        .set_property("--setting-font-size", &format!("{}px", settings.font_size))
        .ok();
    if settings.font_family.trim().is_empty() {
        style.remove_property("--setting-font-text").ok();
    } else {
        let font = settings
            .font_family
            .trim()
            .trim_matches('"')
            .trim_matches('\'');
        style
            .set_property("--setting-font-text", &format!("\"{font}\", {font}"))
            .ok();
    }
    style
        .set_property(
            "--setting-caret-blink",
            if settings.caret_blink {
                "running"
            } else {
                "paused"
            },
        )
        .ok();
    if settings.show_whitespace {
        root.class_list().add_1("mn-show-whitespace").ok();
    } else {
        root.class_list().remove_1("mn-show-whitespace").ok();
    }
    // 折り返しは文書のクラスなので、描く側 (`crate::view`) が現在の設定から決めます。
    // 行番号の幅もここにはありません。設定が言うのは番号を出すかどうかだけで、幅は文書の行数次第なので描く側が決めます。
}

/// ファイルに保存されている設定を書き込みます。1 行に 1 つの `name = value` で、これは TOML の小さなコーナーです。
pub fn write(settings: &Settings) -> String {
    format!(
        "font_size = {}\nfont_family = \"{}\"\ncaret_blink = {}\nwrap = {}\nline_numbers = {}\ncolumn_gap = {}\nhistory_limit = {}\nglobal_shortcut = {}\nshow_whitespace = {}\n",
        settings.font_size,
        settings.font_family.replace('"', ""),
        settings.caret_blink,
        settings.wrap,
        settings.line_numbers,
        settings.column_gap,
        settings.history_limit,
        settings.global_shortcut,
        settings.show_whitespace,
    )
}

/// Reads the settings back, starting from the defaults: a missing or broken
/// line keeps its default, so an old or edited file still opens.
pub fn read(text: &str) -> Settings {
    let mut settings = Settings::default();
    for line in text.lines() {
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        let (name, value) = (name.trim(), value.trim());
        match name {
            "font_size" => {
                if let Ok(size) = value.parse::<f64>() {
                    if (9.0..=48.0).contains(&size) {
                        settings.font_size = size;
                    }
                }
            }
            "font_family" => {
                settings.font_family = value.trim_matches('"').to_string();
            }
            "caret_blink" => {
                if let Ok(blink) = value.parse() {
                    settings.caret_blink = blink;
                }
            }
            "wrap" => {
                if let Ok(wrap) = value.parse() {
                    settings.wrap = wrap;
                }
            }
            "line_numbers" => {
                if let Ok(numbers) = value.parse() {
                    settings.line_numbers = numbers;
                }
            }
            "column_gap" => {
                if let Ok(gap) = value.parse() {
                    settings.column_gap = gap;
                }
            }
            "history_limit" => {
                if let Ok(limit) = value.parse() {
                    settings.history_limit = limit;
                }
            }
            "global_shortcut" => {
                if let Ok(enabled) = value.parse() {
                    settings.global_shortcut = enabled;
                }
            }
            "show_whitespace" => {
                if let Ok(shown) = value.parse() {
                    settings.show_whitespace = shown;
                }
            }
            _ => {}
        }
    }
    settings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_survive_a_round_trip() {
        let settings = Settings {
            font_size: 18.0,
            font_family: "Serif".to_string(),
            caret_blink: false,
            wrap: false,
            line_numbers: true,
            history_limit: 100,
            show_whitespace: true,
            ..Settings::default()
        };
        assert!(read(&write(&settings)) == settings);
    }

    #[test]
    fn a_file_overrides_only_what_it_names() {
        let settings = read(
            "font_size = 18\nfont_family = \"Serif\"\ncaret_blink = false\nhistory_limit = 100\n",
        );
        assert!(settings.font_size == 18.0);
        assert!(settings.font_family == "Serif");
        assert!(!settings.caret_blink);
        assert!(settings.history_limit == 100);
        assert!(settings.column_gap == Settings::default().column_gap);
    }

    #[test]
    fn a_broken_or_old_file_keeps_the_defaults() {
        let settings = read("font_size = large\nnot a line\nnewer_setting = 1\n");
        assert!(settings == Settings::default());
    }
}
