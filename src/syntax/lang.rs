//! 言語定義（LanguageDef）のデータ構造および組み込み言語定義。

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// 言語ごとの構文定義。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanguageDef {
    pub name: String,
    #[serde(default)]
    pub extensions: Vec<String>,
    #[serde(default)]
    pub line_comments: Vec<String>,
    #[serde(default)]
    pub block_comments: Vec<(String, String)>,
    #[serde(default)]
    pub string_delimiters: Vec<String>,
    #[serde(default)]
    pub keywords: HashSet<String>,
    #[serde(default)]
    pub types: HashSet<String>,
    #[serde(default)]
    pub builtins: HashSet<String>,
    #[serde(default)]
    pub constants: HashSet<String>,
    #[serde(default)]
    pub operators: HashSet<String>,
}

impl LanguageDef {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            extensions: Vec::new(),
            line_comments: Vec::new(),
            block_comments: Vec::new(),
            string_delimiters: Vec::new(),
            keywords: HashSet::new(),
            types: HashSet::new(),
            builtins: HashSet::new(),
            constants: HashSet::new(),
            operators: HashSet::new(),
        }
    }

    /// TOML 文字列から言語定義を読み込みます。
    pub fn from_toml(content: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(content)
    }
}

/// 組み込みの 11 言語/フォーマット定義を `languages/*.toml` から読み込みます。
pub fn built_in_languages() -> Vec<LanguageDef> {
    vec![
        LanguageDef::from_toml(include_str!("../../languages/rust.toml")).expect("valid rust.toml"),
        LanguageDef::from_toml(include_str!("../../languages/kotlin.toml"))
            .expect("valid kotlin.toml"),
        LanguageDef::from_toml(include_str!("../../languages/typescript.toml"))
            .expect("valid typescript.toml"),
        LanguageDef::from_toml(include_str!("../../languages/javascript.toml"))
            .expect("valid javascript.toml"),
        LanguageDef::from_toml(include_str!("../../languages/python.toml"))
            .expect("valid python.toml"),
        LanguageDef::from_toml(include_str!("../../languages/toml.toml")).expect("valid toml.toml"),
        LanguageDef::from_toml(include_str!("../../languages/json.toml")).expect("valid json.toml"),
        LanguageDef::from_toml(include_str!("../../languages/html.toml")).expect("valid html.toml"),
        LanguageDef::from_toml(include_str!("../../languages/css.toml")).expect("valid css.toml"),
        LanguageDef::from_toml(include_str!("../../languages/markdown.toml"))
            .expect("valid markdown.toml"),
        LanguageDef::from_toml(include_str!("../../languages/latex.toml"))
            .expect("valid latex.toml"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_languages_load_from_toml() {
        let langs = built_in_languages();
        assert_eq!(langs.len(), 11);
        let rust = langs.iter().find(|l| l.name == "Rust").unwrap();
        assert!(rust.keywords.contains("fn"));
        assert!(rust.extensions.contains(&"rs".to_string()));

        let kt = langs.iter().find(|l| l.name == "Kotlin").unwrap();
        assert!(kt.keywords.contains("fun"));
        assert!(kt.keywords.contains("public"));

        let md = langs.iter().find(|l| l.name == "Markdown").unwrap();
        assert!(md.extensions.contains(&"md".to_string()));
    }
}
