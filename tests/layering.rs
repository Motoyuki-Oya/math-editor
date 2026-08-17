//! Keeps the layers apart, by reading the source instead of trusting review.
//!
//! The display and the file format must never know about each other: the
//! notation has to be readable without a screen, and nothing drawn may be
//! derived from the way it is written. Both are allowed to know `structure`,
//! which is the meaning they share.

use std::fs;
use std::path::{Path, PathBuf};

/// A layer, the things it may not mention, and whether its tests are held to
/// the same rule.
struct Rule {
    dir: &'static str,
    forbidden: &'static [&'static str],
    /// Fixtures in tests may reach for the notation, since writing a structure
    /// out is the clearest way to write one down.
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

/// The source without its tests, which sit at the end of the file by convention.
fn without_tests(source: &str) -> &str {
    match source.find("#[cfg(test)]") {
        Some(at) => &source[..at],
        None => source,
    }
}

/// The code alone: comments name the other layers when they explain why they
/// are out of reach, which is not a dependency.
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

/// The text of every string literal, which is where notation would show up.
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

/// The format turns whole documents into files and back, and nothing else.
///
/// Copying used to go through it, which put the notation on the clipboard: a
/// fraction came out as `$(a/b)`, in another program and in this one. Keeping
/// the entry points to `read` and `write` is what stops that from coming back —
/// there is nothing here for anything but a file to call.
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
        ["read", "write"],
        "{} hands out more than a file needs",
        file.display(),
    );
}

/// The format is the only place the notation `$( … )` is written or read.
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
