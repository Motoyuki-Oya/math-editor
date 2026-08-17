//! The user's settings: one place for every default, and the file that
//! overrides them.
//!
//! The saved notation is not a setting — it is the file format itself, and
//! making it configurable would make files unreadable elsewhere. The DOM
//! classes the view uses are not settings either; they are its own contract.

use std::cell::RefCell;

use wasm_bindgen::JsCast;

/// Everything the user can change. What the settings window shows is a
/// subset: values the user has no reason to touch stay in the file only.
#[derive(Clone, PartialEq)]
pub struct Settings {
    /// The size of the text in the editor, in pixels.
    pub font_size: f64,
    /// The font of the text. Empty means the built-in default.
    pub font_family: String,
    /// Whether the caret blinks.
    pub caret_blink: bool,
    /// Whether a line too long for the window is carried on underneath
    /// instead of running off the side.
    pub wrap: bool,
    /// Whether each line shows its number.
    pub line_numbers: bool,
    /// The gap between aligned columns, in pixels. File only.
    pub column_gap: f64,
    /// How many steps the undo history keeps. File only.
    pub history_limit: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            font_size: 15.0,
            font_family: String::new(),
            caret_blink: true,
            wrap: true,
            line_numbers: false,
            column_gap: 18.0,
            history_limit: 500,
        }
    }
}

thread_local! {
    static CURRENT: RefCell<Settings> = RefCell::new(Settings::default());
}

/// The settings in effect right now.
pub fn current() -> Settings {
    CURRENT.with(|current| current.borrow().clone())
}

pub fn column_gap() -> f64 {
    CURRENT.with(|current| current.borrow().column_gap)
}

pub fn line_numbers() -> bool {
    CURRENT.with(|current| current.borrow().line_numbers)
}

pub fn history_limit() -> usize {
    CURRENT.with(|current| current.borrow().history_limit)
}

/// Makes `settings` the ones in effect, showing the visual ones on screen.
pub fn apply(settings: Settings) {
    show(&settings);
    CURRENT.with(|current| *current.borrow_mut() = settings);
}

/// Visual settings reach the screen as CSS variables on the document root,
/// so the stylesheet stays the only place that decides how things look.
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
    if settings.font_family.is_empty() {
        style.remove_property("--setting-font-text").ok();
    } else {
        style
            .set_property("--setting-font-text", &settings.font_family)
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
    style
        .set_property(
            "--setting-wrap",
            if settings.wrap { "pre-wrap" } else { "pre" },
        )
        .ok();
    // A wrapped line is as wide as the window; an unwrapped one is as wide as
    // it needs, which is what gives the editor something to scroll sideways.
    style
        .set_property(
            "--setting-line-width",
            if settings.wrap { "100%" } else { "max-content" },
        )
        .ok();
    style
        .set_property(
            "--setting-gutter",
            if settings.line_numbers {
                "3.5em"
            } else {
                "0px"
            },
        )
        .ok();
}

/// Writes the settings as the file keeps them: one `name = value` per line,
/// which is a small corner of TOML.
pub fn write(settings: &Settings) -> String {
    format!(
        "font_size = {}\nfont_family = \"{}\"\ncaret_blink = {}\nwrap = {}\nline_numbers = {}\ncolumn_gap = {}\nhistory_limit = {}\n",
        settings.font_size,
        settings.font_family.replace('"', ""),
        settings.caret_blink,
        settings.wrap,
        settings.line_numbers,
        settings.column_gap,
        settings.history_limit,
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
                if let Ok(size) = value.parse() {
                    settings.font_size = size;
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
