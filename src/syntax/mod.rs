//! 構文ハイライトおよび言語定義管理モジュール。

pub mod lang;
pub mod lexer;

use std::cell::RefCell;
use std::collections::HashMap;

use self::lang::{built_in_languages, LanguageDef};
pub use self::lexer::{tokenize_line, TokenKind, TokenSpan};

/// インライン補完（ゴーストテキスト / Phantom Text）の情報。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GhostText {
    /// 補完対象の行番号
    pub line: usize,
    /// キャレットの列位置 (文字単位)
    pub col: usize,
    /// 入力されたプレフィックス (例: "pu")
    pub prefix: String,
    /// キャレットの後方に薄く表示する残余文字列 (例: "blic")
    pub suffix: String,
    /// 補完される完全な単語 (例: "public")
    pub full: String,
    /// 構造化（ルビ）補完情報: Some((対象漢字長, 読み文字列))
    pub ruby: Option<(usize, String)>,
}

thread_local! {
    static REGISTRY: RefCell<Registry> = RefCell::new(Registry::new());
}

pub struct Registry {
    languages: Vec<LanguageDef>,
    by_extension: HashMap<String, usize>,
    by_name: HashMap<String, usize>,
}

impl Registry {
    pub fn new() -> Self {
        let mut reg = Self {
            languages: Vec::new(),
            by_extension: HashMap::new(),
            by_name: HashMap::new(),
        };
        for def in built_in_languages() {
            reg.register(def);
        }
        reg
    }

    pub fn register(&mut self, def: LanguageDef) {
        let index = self.languages.len();
        self.by_name.insert(def.name.to_lowercase(), index);
        for ext in &def.extensions {
            self.by_extension.insert(ext.to_lowercase(), index);
        }
        self.languages.push(def);
    }

    pub fn find_by_path(&self, path: &str) -> Option<&LanguageDef> {
        let ext = path.rsplit('.').next()?.to_lowercase();
        let &index = self.by_extension.get(&ext)?;
        self.languages.get(index)
    }

    pub fn find_by_name(&self, name: &str) -> Option<&LanguageDef> {
        let &index = self.by_name.get(&name.to_lowercase())?;
        self.languages.get(index)
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

/// ファイルパスまたは拡張子から適切な言語定義を取得します。
pub fn for_path(path: &str) -> Option<LanguageDef> {
    REGISTRY.with(|reg| reg.borrow().find_by_path(path).cloned())
}

/// 言語名（"Rust", "Kotlin" 等）から言語定義を取得します。
pub fn for_name(name: &str) -> Option<LanguageDef> {
    REGISTRY.with(|reg| reg.borrow().find_by_name(name).cloned())
}

/// ユーザー定義の言語定義を登録・上書きします。
pub fn register_language(def: LanguageDef) {
    REGISTRY.with(|reg| reg.borrow_mut().register(def));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_lookup() {
        let rust = for_path("src/main.rs").expect("Rust should be found for .rs");
        assert_eq!(rust.name, "Rust");

        let kotlin = for_path("App.kt").expect("Kotlin should be found for .kt");
        assert_eq!(kotlin.name, "Kotlin");

        let ts = for_path("index.tsx").expect("TypeScript should be found for .tsx");
        assert_eq!(ts.name, "TypeScript");

        let py = for_path("script.py").expect("Python should be found for .py");
        assert_eq!(py.name, "Python");

        let md = for_path("README.md").expect("Markdown should be found for .md");
        assert_eq!(md.name, "Markdown");
    }
}
