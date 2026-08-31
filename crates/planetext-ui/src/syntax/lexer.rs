//! 行単位の高速字句解析器（Lexer）。

use super::lang::LanguageDef;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    Keyword,
    Type,
    String,
    Comment,
    Builtin,
    Constant,
    Operator,
    Punctuation,
}

impl TokenKind {
    pub fn class_name(&self) -> &'static str {
        match self {
            TokenKind::Keyword => "mn-syn-keyword",
            TokenKind::Type => "mn-syn-type",
            TokenKind::String => "mn-syn-string",
            TokenKind::Comment => "mn-syn-comment",
            TokenKind::Builtin => "mn-syn-builtin",
            TokenKind::Constant => "mn-syn-constant",
            TokenKind::Operator => "mn-syn-operator",
            TokenKind::Punctuation => "mn-syn-punct",
        }
    }

    pub fn from_str_name(name: &str) -> TokenKind {
        match name.to_lowercase().as_str() {
            "keyword" => TokenKind::Keyword,
            "type" => TokenKind::Type,
            "string" => TokenKind::String,
            "comment" => TokenKind::Comment,
            "builtin" => TokenKind::Builtin,
            "constant" => TokenKind::Constant,
            "operator" => TokenKind::Operator,
            "punctuation" | "punct" => TokenKind::Punctuation,
            _ => TokenKind::Keyword,
        }
    }
}

/// 行内の文字インデックス範囲とトークン種類。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenSpan {
    pub start: usize,
    pub end: usize,
    pub kind: TokenKind,
}

fn word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

fn configured_literal(slice: &str, lang: &LanguageDef) -> Option<(usize, TokenKind)> {
    [
        (&lang.keywords, TokenKind::Keyword),
        (&lang.types, TokenKind::Type),
        (&lang.builtins, TokenKind::Builtin),
        (&lang.constants, TokenKind::Constant),
    ]
    .into_iter()
    .flat_map(|(words, kind)| words.iter().map(move |word| (word, kind)))
    .filter_map(|(word, kind)| {
        let width = word.chars().count();
        let has_symbol = word.chars().any(|c| !word_char(c));
        let boundary = word.chars().next_back().is_none_or(|last| !word_char(last))
            || slice.chars().nth(width).is_none_or(|next| !word_char(next));
        (has_symbol && boundary && slice.starts_with(word)).then_some((width, kind))
    })
    .max_by_key(|(width, _)| *width)
}

fn configured_operator(slice: &str, lang: &LanguageDef) -> Option<usize> {
    lang.operators
        .iter()
        .filter(|operator| !operator.is_empty() && slice.starts_with(operator.as_str()))
        .map(|operator| operator.chars().count())
        .max()
}

/// 1 行のテキストを、指定された言語定義に基づいてトークン分割します。
pub fn tokenize_line(line: &str, lang: &LanguageDef) -> Vec<TokenSpan> {
    if line.trim().is_empty() {
        return Vec::new();
    }

    let mut spans = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        // 1. 空白文字のスキップ
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }

        let slice_from_i: String = chars[i..].iter().collect();

        // 1.5. 汎用行頭ルール (lang.line_rules: Markdownの見出し、コードフェンス等)
        if chars[..i].iter().all(|c| c.is_whitespace()) {
            let mut matched_line_rule = false;
            for rule in &lang.line_rules {
                if slice_from_i.starts_with(&rule.prefix) {
                    let kind = TokenKind::from_str_name(&rule.kind);
                    if rule.whole_line {
                        spans.push(TokenSpan {
                            start: i,
                            end: len,
                            kind,
                        });
                        return spans;
                    } else {
                        let prefix_len = rule.prefix.chars().count();
                        spans.push(TokenSpan {
                            start: i,
                            end: i + prefix_len,
                            kind,
                        });
                        i += prefix_len;
                        matched_line_rule = true;
                        break;
                    }
                }
            }
            if matched_line_rule {
                continue;
            }
        }

        // 1.6. 汎用囲みルール (lang.enclosures: TOMLの [section], [[array]] 等)
        let mut matched_enclosure = false;
        for enc in &lang.enclosures {
            if slice_from_i.starts_with(&enc.open) {
                let open_len = enc.open.chars().count();
                let inner = &slice_from_i[open_len..];
                if let Some(pos) = inner.find(&enc.close) {
                    let total_chars =
                        open_len + inner[..pos].chars().count() + enc.close.chars().count();
                    spans.push(TokenSpan {
                        start: i,
                        end: i + total_chars,
                        kind: TokenKind::from_str_name(&enc.kind),
                    });
                    i += total_chars;
                    matched_enclosure = true;
                    break;
                }
            }
        }
        if matched_enclosure {
            continue;
        }

        // 2. 行コメントの判定
        for lc in &lang.line_comments {
            if slice_from_i.starts_with(lc) {
                spans.push(TokenSpan {
                    start: i,
                    end: len,
                    kind: TokenKind::Comment,
                });
                return spans;
            }
        }

        // 3. ブロックコメントの判定 (同一行内)
        let mut matched_block = false;
        for (open, close) in &lang.block_comments {
            if slice_from_i.starts_with(open) {
                matched_block = true;
                let start = i;
                i += open.chars().count();
                let remaining: String = chars[i..].iter().collect();
                if let Some(pos) = remaining.find(close) {
                    let close_char_len = remaining[..pos].chars().count() + close.chars().count();
                    i += close_char_len;
                } else {
                    i = len;
                }
                spans.push(TokenSpan {
                    start,
                    end: i,
                    kind: TokenKind::Comment,
                });
                break;
            }
        }
        if matched_block {
            continue;
        }

        // 4. 文字列リテラルの判定
        let mut matched_str = false;
        for delim in &lang.string_delimiters {
            if slice_from_i.starts_with(delim) {
                matched_str = true;
                let start = i;
                let delim_char_len = delim.chars().count();
                i += delim_char_len;
                let mut escaped = false;
                while i < len {
                    let c = chars[i];
                    if escaped {
                        escaped = false;
                        i += 1;
                        continue;
                    }
                    if c == '\\' {
                        escaped = true;
                        i += 1;
                        continue;
                    }
                    let rem: String = chars[i..].iter().collect();
                    if rem.starts_with(delim) {
                        i += delim_char_len;
                        break;
                    }
                    i += 1;
                }
                spans.push(TokenSpan {
                    start,
                    end: i,
                    kind: TokenKind::String,
                });
                break;
            }
        }
        if matched_str {
            continue;
        }

        if let Some((width, kind)) = configured_literal(&slice_from_i, lang) {
            spans.push(TokenSpan {
                start: i,
                end: i + width,
                kind,
            });
            i += width;
            continue;
        }

        // 8. 識別子・キーワード・型の判定
        if chars[i].is_alphabetic() || chars[i] == '_' || chars[i] == '$' {
            let start = i;
            while i < len && word_char(chars[i]) {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();

            let kind = if lang.keywords.contains(&word) {
                TokenKind::Keyword
            } else if lang.types.contains(&word) {
                TokenKind::Type
            } else if lang.constants.contains(&word) {
                TokenKind::Constant
            } else if lang.builtins.contains(&word) {
                TokenKind::Builtin
            } else {
                // 通常の識別子は色付けなし（プレーンテキスト）
                continue;
            };

            spans.push(TokenSpan {
                start,
                end: i,
                kind,
            });
            continue;
        }

        if let Some(width) = configured_operator(&slice_from_i, lang) {
            spans.push(TokenSpan {
                start: i,
                end: i + width,
                kind: TokenKind::Operator,
            });
            i += width;
        } else {
            i += 1;
        }
    }

    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::lang::built_in_languages;

    #[test]
    fn symbol_words_and_operators_come_only_from_the_language_definition() {
        let mut language = LanguageDef::new("Custom");
        language.builtins.insert("\\known".into());
        language.operators.insert("=".into());
        language.operators.insert("==".into());

        let tokens = tokenize_line("\\known == \\unknown", &language);
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0].kind, TokenKind::Builtin);
        assert_eq!((tokens[1].start, tokens[1].end), (7, 9));
        assert_eq!(tokens[1].kind, TokenKind::Operator);
    }

    #[test]
    fn numbers_are_not_highlighted_in_any_language() {
        for language in built_in_languages() {
            assert!(
                tokenize_line("42 3.14 0xff", &language).is_empty(),
                "{} highlighted a number",
                language.name
            );
        }
    }

    #[test]
    fn test_rust_tokenization() {
        let langs = built_in_languages();
        let rust = langs.iter().find(|l| l.name == "Rust").unwrap();
        let line = "pub fn calculate_total(x: usize) -> u32 { // comment";
        let tokens = tokenize_line(line, rust);
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Keyword)); // pub, fn
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Type)); // usize, u32
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Comment)); // // comment
    }

    #[test]
    fn test_kotlin_tokenization() {
        let langs = built_in_languages();
        let kt = langs.iter().find(|l| l.name == "Kotlin").unwrap();
        let line = "public fun getName(): String = \"hello\"";
        let tokens = tokenize_line(line, kt);
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Keyword)); // public, fun
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Type)); // String
        assert!(tokens.iter().any(|t| t.kind == TokenKind::String)); // "hello"
    }

    #[test]
    fn test_latex_tokenization() {
        let langs = built_in_languages();
        let latex = langs.iter().find(|l| l.name == "LaTeX").unwrap();
        let line = "\\begin{equation} \\frac{a}{b} + \\alpha % formula";
        let tokens = tokenize_line(line, latex);
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Keyword)); // \begin
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Builtin)); // \frac, \alpha
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Comment)); // % formula
    }

    #[test]
    fn test_python_tokenization() {
        let langs = built_in_languages();
        let py = langs.iter().find(|l| l.name == "Python").unwrap();
        let line = "def calculate(x: int) -> str: # comment";
        let tokens = tokenize_line(line, py);
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Keyword)); // def
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Type)); // int, str
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Comment)); // # comment
    }

    #[test]
    fn test_typescript_tokenization() {
        let langs = built_in_languages();
        let ts = langs.iter().find(|l| l.name == "TypeScript").unwrap();
        let line = "export interface User { id: number; name: string; }";
        let tokens = tokenize_line(line, ts);
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Keyword)); // export, interface
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Type)); // number, string
    }

    #[test]
    fn test_toml_and_json_tokenization() {
        let langs = built_in_languages();
        let toml = langs.iter().find(|l| l.name == "TOML").unwrap();
        let line = "title = \"Planetext\" # app name";
        let tokens = tokenize_line(line, toml);
        assert!(tokens.iter().any(|t| t.kind == TokenKind::String));
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Comment));

        let json = langs.iter().find(|l| l.name == "JSON").unwrap();
        let line = "{\"active\": true, \"count\": 42}";
        let tokens = tokenize_line(line, json);
        assert!(tokens.iter().any(|t| t.kind == TokenKind::String));
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Constant)); // true
        let number = line.find("42").unwrap();
        assert!(!tokens
            .iter()
            .any(|token| token.start <= number && number < token.end));
    }

    #[test]
    fn test_html_and_css_tokenization() {
        let langs = built_in_languages();
        let html = langs.iter().find(|l| l.name == "HTML").unwrap();
        let line = "<div class=\"app\">Hello <!-- comment --></div>";
        let tokens = tokenize_line(line, html);
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Keyword)); // div
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Builtin)); // class
        assert!(tokens.iter().any(|t| t.kind == TokenKind::String)); // "app"
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Comment)); // <!-- comment -->

        let css = langs.iter().find(|l| l.name == "CSS").unwrap();
        let line = ".app { color: red; font-size: 15px; } /* style */";
        let tokens = tokenize_line(line, css);
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Constant)); // color, font-size
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Type)); // px
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Comment)); // /* style */
    }

    #[test]
    fn test_markdown_tokenization() {
        let langs = built_in_languages();
        let md = langs.iter().find(|l| l.name == "Markdown").unwrap();
        let line1 = "# 単なるテキストエディタ";
        let tokens1 = tokenize_line(line1, md);
        assert_eq!(tokens1.len(), 1);
        assert_eq!(tokens1[0].kind, TokenKind::Keyword);
        assert_eq!(tokens1[0].end, line1.chars().count());

        let line2 = "## 概要";
        let tokens2 = tokenize_line(line2, md);
        assert_eq!(tokens2.len(), 1);
        assert_eq!(tokens2[0].kind, TokenKind::Keyword);

        let line3 = "```kotlin";
        let tokens3 = tokenize_line(line3, md);
        assert_eq!(tokens3.len(), 1);
        assert_eq!(tokens3[0].kind, TokenKind::Builtin);

        let list = tokenize_line("- item", md);
        assert_eq!(list.len(), 1);
        assert_eq!((list[0].start, list[0].end), (0, 2));
        assert!(tokenize_line("49 + 81", md).is_empty());
    }

    #[test]
    fn test_toml_sections() {
        let langs = built_in_languages();
        let toml = langs.iter().find(|l| l.name == "TOML").unwrap();
        let line1 = "[package]";
        let tokens1 = tokenize_line(line1, toml);
        assert_eq!(tokens1.len(), 1);
        assert_eq!(tokens1[0].kind, TokenKind::Type);

        let line2 = "[[bin]]";
        let tokens2 = tokenize_line(line2, toml);
        assert_eq!(tokens2.len(), 1);
        assert_eq!(tokens2[0].kind, TokenKind::Type);
    }
}
