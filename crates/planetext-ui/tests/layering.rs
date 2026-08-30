//! レビューを信頼するのではなくソースを読み取ることによって、レイヤーを分離したままにします。
//!
//! 表示とファイル形式は決して相互に認識してはなりません。表記は画面なしで読める必要があり、描画方法から派生したものは何もありません。どちらも「構造」を知ることができます。これは、それらが共有する意味です。

use std::fs;
use std::path::{Path, PathBuf};

/// レイヤー、そのレイヤーで言及してはいけないこと、およびそのテストが同じルールに従うかどうか。
struct Rule {
    dir: &'static str,
    forbidden: &'static [&'static str],
    /// 構造を書き出すことが構造を書き留める最も明確な方法であるため、テスト内のフィクスチャは表記法に到達する可能性があります。
    tests_too: bool,
}

const RULES: &[Rule] = &[
    Rule {
        dir: "structure",
        forbidden: &[
            "crate::format",
            "crate::view",
            "crate::editor",
            "crate::settings",
            "web_sys",
            "wasm_bindgen",
            "leptos",
        ],
        tests_too: false,
    },
    Rule {
        dir: "format",
        forbidden: &[
            "crate::view",
            "crate::editor",
            "crate::settings",
            "web_sys",
            "wasm_bindgen",
            "leptos",
        ],
        tests_too: true,
    },
    Rule {
        dir: "view",
        forbidden: &["crate::format", "crate::editor"],
        tests_too: true,
    },
];

fn sources(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).expect("a layer directory") {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() {
            out.extend(sources(&path));
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
    out
}

/// テストのないソース。慣例によりファイルの最後に配置されます。
fn without_tests(source: &str) -> &str {
    match source.find("#[cfg(test)]") {
        Some(at) => &source[..at],
        None => source,
    }
}

/// コードのみ: コメントは、その理由を説明するときに他のレイヤーに名前を付けます。
fn without_comments(source: &str) -> String {
    source
        .lines()
        .map(|line| match line.find("//") {
            Some(at) => &line[..at],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// すべての文字列リテラルのテキスト。これが表記法が表示される場所です。
fn string_literals(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = source.chars();
    while let Some(c) = chars.next() {
        if c != '"' {
            continue;
        }
        let mut literal = String::new();
        loop {
            match chars.next() {
                Some('\\') => {
                    chars.next();
                }
                Some('"') | None => break,
                Some(c) => literal.push(c),
            }
        }
        out.push(literal);
    }
    out
}

#[test]
fn the_layers_do_not_reach_into_each_other() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for rule in RULES {
        let dir = root.join(rule.dir);
        let files = sources(&dir);
        assert!(!files.is_empty(), "{} has no sources", rule.dir);
        for file in files {
            let source = fs::read_to_string(&file).expect("a source file");
            let checked = without_comments(if rule.tests_too {
                source.as_str()
            } else {
                without_tests(&source)
            });
            for name in rule.forbidden {
                assert!(
                    !checked.contains(name),
                    "{} mentions {name}, which the {} layer may not depend on",
                    file.display(),
                    rule.dir,
                );
            }
        }
    }
}

/// この形式はファイルの読み書きだけに使われ、それ以外は何も変換しません。
///
/// 以前はコピーがそれを通過し、表記法がクリップボードに置かれていました。別のプログラムでもこのプログラムでも、分数は `$(a/b)` として出力されました。エントリ ポイントをファイルの読み書き（全体の `read` / `write` と、範囲読みの読み込みが使う行単位の `read_line`）に維持することが、この問題の再発を防ぐ方法です。ここには、呼び出すファイル以外に何もありません。
#[test]
fn the_format_only_converts_whole_documents() {
    let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/format/document.rs");
    let source = fs::read_to_string(&file).expect("the document format");
    let names: Vec<&str> = without_tests(&source)
        .lines()
        .filter_map(|line| line.trim().strip_prefix("pub fn "))
        .filter_map(|rest| rest.split('(').next())
        .collect();
    assert_eq!(
        names,
        ["read", "read_line", "write", "write_line"],
        "{} hands out more than a file needs",
        file.display(),
    );
}

/// この形式は、表記法 `$( … )` が書き込まれたり読み取られたりする唯一の場所です。
#[test]
fn only_the_format_knows_the_notation() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for dir in ["structure", "view", "editor"] {
        for file in sources(&root.join(dir)) {
            let source = fs::read_to_string(&file).expect("a source file");
            for literal in string_literals(without_tests(&source)) {
                assert!(
                    !literal.contains("$("),
                    "{} writes the notation itself; leave that to the format layer",
                    file.display(),
                );
            }
        }
    }
}

#[test]
fn framework_specific_apis_stop_at_the_connectors() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let frontend = root.join("src");
    let connector = frontend.join("framework/tauri.rs");
    for file in sources(&frontend) {
        let source = without_comments(
            &fs::read_to_string(&file).unwrap_or_else(|_| panic!("cannot read {}", file.display())),
        );
        if file != connector {
            assert!(
                !source.contains("__TAURI__"),
                "{} reaches through the framework connector",
                file.display(),
            );
        }
        if file.starts_with(frontend.join("app")) {
            assert!(
                !source.contains("framework::tauri"),
                "{} selects a concrete framework implementation",
                file.display(),
            );
        }
    }

    let document = root.join("../planetext-document");
    let manifest = fs::read_to_string(document.join("Cargo.toml")).expect("the document manifest");
    for name in ["tauri", "wry", "gpui", "arboard", "tokio"] {
        assert!(
            !manifest.to_ascii_lowercase().contains(name),
            "planetext-document depends on {name}",
        );
    }
    for file in sources(&document.join("src")) {
        let source = fs::read_to_string(&file).expect("a document source file");
        for name in ["tauri", "wry", "gpui", "arboard", "tokio"] {
            assert!(
                !without_comments(&source)
                    .to_ascii_lowercase()
                    .contains(name),
                "{} reaches into {name}",
                file.display(),
            );
        }
    }
}
