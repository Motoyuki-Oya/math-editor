//! The document surface: ordinary text editing handled by the browser, with
//! formulas embedded as islands the browser does not touch.

use std::cell::RefCell;

use js_sys::RegExp;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{Document, Element, HtmlElement, Node, Range, Selection};

use crate::markdown::{self, Segment};
use crate::math::commands;
use crate::math::edit::Escape;
use crate::math::field::{self, FIELD_CLASS};

thread_local! {
    static HOST: RefCell<Option<HtmlElement>> = const { RefCell::new(None) };
    static ON_CHANGE: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
    static HISTORY: RefCell<History> = RefCell::new(History::default());
    /// Set while the document is being rebuilt, so restoring is not recorded.
    static RESTORING: RefCell<bool> = const { RefCell::new(false) };
}

/// One editing state of the whole document, formulas included.
#[derive(Clone, Default)]
struct Snapshot {
    text: String,
    caret: usize,
}

#[derive(Default)]
struct History {
    past: Vec<Snapshot>,
    future: Vec<Snapshot>,
    present: Snapshot,
    stamp: f64,
}

const COALESCE_MS: f64 = 700.0;
const HISTORY_LIMIT: usize = 500;

fn restoring() -> bool {
    RESTORING.with(|flag| *flag.borrow())
}

/// Remembers the document as it stands, coalescing a burst of typing into one
/// undo step the way a text editor does.
fn record() {
    if restoring() {
        return;
    }
    let text = to_markdown();
    let caret = caret_units().unwrap_or(0);
    let now = js_sys::Date::now();
    HISTORY.with(|history| {
        let history = &mut *history.borrow_mut();
        if history.present.text == text {
            history.present.caret = caret;
            return;
        }
        if now - history.stamp > COALESCE_MS {
            history.past.push(history.present.clone());
            if history.past.len() > HISTORY_LIMIT {
                history.past.remove(0);
            }
        }
        history.present = Snapshot { text, caret };
        history.stamp = now;
        history.future.clear();
    });
}

/// Forgets the history, for when a different document is loaded.
fn reset_history(text: String) {
    HISTORY.with(|history| {
        *history.borrow_mut() = History {
            past: Vec::new(),
            future: Vec::new(),
            present: Snapshot { text, caret: 0 },
            stamp: 0.0,
        };
    });
}

pub fn undo() -> bool {
    let restore = HISTORY.with(|history| {
        let history = &mut *history.borrow_mut();
        let previous = history.past.pop()?;
        history.future.push(history.present.clone());
        history.present = previous.clone();
        history.stamp = 0.0;
        Some(previous)
    });
    apply(restore)
}

pub fn redo() -> bool {
    let restore = HISTORY.with(|history| {
        let history = &mut *history.borrow_mut();
        let next = history.future.pop()?;
        history.past.push(history.present.clone());
        history.present = next.clone();
        history.stamp = 0.0;
        Some(next)
    });
    apply(restore)
}

fn apply(snapshot: Option<Snapshot>) -> bool {
    let Some(snapshot) = snapshot else {
        return false;
    };
    RESTORING.with(|flag| *flag.borrow_mut() = true);
    load(&snapshot.text);
    place_caret(snapshot.caret);
    RESTORING.with(|flag| *flag.borrow_mut() = false);
    notify_change();
    true
}

fn document() -> Option<Document> {
    web_sys::window()?.document()
}

fn host() -> Option<HtmlElement> {
    HOST.with(|host| host.borrow().clone())
}

pub fn set_on_change(callback: Box<dyn Fn()>) {
    ON_CHANGE.with(|slot| *slot.borrow_mut() = Some(callback));
}

pub fn notify_change() {
    record();
    ON_CHANGE.with(|slot| {
        if let Some(callback) = slot.borrow().as_ref() {
            callback();
        }
    });
}

/// Prepares the editing surface and keeps formula islands alive across the
/// DOM changes the browser makes on its own (typing, undo, paste).
pub fn init(element: &HtmlElement) {
    element.set_attribute("contenteditable", "true").ok();
    element.set_attribute("spellcheck", "false").ok();
    HOST.with(|host| *host.borrow_mut() = Some(element.clone()));

    let on_input = Closure::<dyn FnMut()>::new(move || {
        attach_new_fields();
        notify_change();
    });
    element
        .add_event_listener_with_callback("input", on_input.as_ref().unchecked_ref())
        .ok();
    on_input.forget();

    let element_for_keys = element.clone();
    let on_keydown =
        Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(move |event: web_sys::KeyboardEvent| {
            if event.ctrl_key() || event.meta_key() || event.alt_key() || event.is_composing() {
                return;
            }
            handle_caret_into_field(&element_for_keys, &event);
            handle_math_trigger(&element_for_keys, &event);
        });
    element
        .add_event_listener_with_callback("keydown", on_keydown.as_ref().unchecked_ref())
        .ok();
    on_keydown.forget();

    // The browser's own history knows nothing about the formulas, so the
    // document keeps its own and the built-in one is turned away.
    let on_before_input = Closure::<dyn FnMut(web_sys::InputEvent)>::new(
        move |event: web_sys::InputEvent| match event.input_type().as_str() {
            "historyUndo" => {
                event.prevent_default();
                undo();
            }
            "historyRedo" => {
                event.prevent_default();
                redo();
            }
            _ => {}
        },
    );
    element
        .add_event_listener_with_callback("beforeinput", on_before_input.as_ref().unchecked_ref())
        .ok();
    on_before_input.forget();

    if element.child_nodes().length() == 0 {
        load("");
    }
}

/// Gives keyboard focus to a formula when the caret walks into it from the
/// surrounding text.
fn handle_caret_into_field(host: &HtmlElement, event: &web_sys::KeyboardEvent) {
    let key = event.key();
    let forward = match key.as_str() {
        "ArrowRight" => true,
        "ArrowLeft" => false,
        _ => return,
    };
    let Some(selection) = window_selection() else {
        return;
    };
    if !selection.is_collapsed() {
        return;
    }
    let Some(node) = selection.anchor_node() else {
        return;
    };
    if !host.contains(Some(&node)) {
        return;
    }
    let offset = selection.anchor_offset();
    let at_edge = match node.node_type() {
        Node::TEXT_NODE => {
            let len = node.text_content().map(|t| t.chars().count()).unwrap_or(0) as u32;
            if forward {
                offset >= len
            } else {
                offset == 0
            }
        }
        _ => true,
    };
    if !at_edge {
        return;
    }
    let neighbour = if forward {
        next_element(&node)
    } else {
        previous_element(&node)
    };
    let Some(neighbour) = neighbour else { return };
    if !neighbour.class_list().contains(FIELD_CLASS) {
        return;
    }
    let Ok(neighbour) = neighbour.dyn_into::<HtmlElement>() else {
        return;
    };
    event.prevent_default();
    if forward {
        field::focus_at_start(&neighbour);
    } else {
        field::focus_at_end(&neighbour);
    }
}

/// Starts a formula straight from the text, so `1/`, `x^` or `\sqrt ` switch
/// into math without a menu, the way Markdown shortcuts work. Only shapes
/// plain text cannot hold become formulas; `\alpha ` is simply `α`.
fn handle_math_trigger(host: &HtmlElement, event: &web_sys::KeyboardEvent) {
    if event.default_prevented() {
        return;
    }
    let key = event.key();
    let caret = caret_in_text(host);
    let before = caret
        .as_ref()
        .map(|(node, offset)| prefix_utf16(&node.text_content().unwrap_or_default(), *offset))
        .unwrap_or_default();

    let (consume, seed): (usize, Seed) = match key.as_str() {
        "$" => (0, Seed::Empty),
        "/" | "^" | "_" => {
            let run = trailing_run(&before);
            // `and/or` should stay prose; `1/`, `x/` and `x^` are formulas.
            let mathlike = !run.is_empty()
                && (key != "/"
                    || run.chars().any(|c| c.is_ascii_digit())
                    || run.chars().count() == 1);
            if !mathlike {
                return;
            }
            (
                run.encode_utf16().count(),
                Seed::Typed(run, key.chars().next().unwrap()),
            )
        }
        " " => match trailing_shortcut(&before) {
            Some((consumed, seed)) => (consumed, seed),
            None => return,
        },
        _ => return,
    };

    event.prevent_default();
    // Symbols and function names are ordinary text, so they never open a field.
    if let Seed::Text(text) = &seed {
        if let Some((node, offset)) = caret {
            replace_with_text(host, &node, offset, consume, text);
        }
        notify_change();
        return;
    }
    let field_host = match caret {
        Some((node, offset)) => replace_with_field(host, &node, offset, consume),
        // An empty line has no text node to cut from, so just open a formula.
        None => {
            insert_math(false);
            field::focused_host()
        }
    };
    let Some(field_host) = field_host else { return };
    match seed {
        Seed::Empty => {}
        Seed::Typed(run, trigger) => {
            for c in run.chars() {
                field::type_char(&field_host, c);
            }
            field::type_char(&field_host, trigger);
        }
        Seed::Node(node) => {
            field::insert_into_focused(node);
        }
        // Handled above, before any field was created.
        Seed::Text(_) => {}
    }
    notify_change();
}

enum Seed {
    /// `$`: an empty formula.
    Empty,
    /// `1/`, `x^`: the text that was already typed, then the trigger.
    Typed(String, char),
    /// `\sqrt `: the structure the command names.
    Node(crate::math::ast::Node),
    /// `\alpha `, `\sin `: plain text, no formula needed.
    Text(String),
}

/// Replaces the `consume` code units before the caret with a formula field.
fn replace_with_field(
    host: &HtmlElement,
    node: &Node,
    offset: u32,
    consume: usize,
) -> Option<HtmlElement> {
    let doc = document()?;
    let range = doc.create_range().ok()?;
    range.set_start(node, offset - consume as u32).ok()?;
    range.set_end(node, offset).ok()?;
    range.delete_contents().ok()?;
    let element = field::create_element(&doc, "", false);
    range.insert_node(&element).ok()?;
    let element = element.dyn_into::<HtmlElement>().ok()?;
    field::attach(&element);
    field::focus_at_end(&element);
    host.normalize();
    Some(element)
}

/// Replaces the `consume` code units before the caret with plain text.
fn replace_with_text(
    host: &HtmlElement,
    node: &Node,
    offset: u32,
    consume: usize,
    text: &str,
) -> Option<()> {
    let doc = document()?;
    let range = doc.create_range().ok()?;
    range.set_start(node, offset - consume as u32).ok()?;
    range.set_end(node, offset).ok()?;
    range.delete_contents().ok()?;
    let inserted = doc.create_text_node(text);
    range.insert_node(&inserted).ok()?;
    let selection = window_selection()?;
    selection
        .collapse_with_offset(Some(&inserted), text.encode_utf16().count() as u32)
        .ok()?;
    host.normalize();
    Some(())
}

fn caret_in_text(host: &HtmlElement) -> Option<(Node, u32)> {
    let selection = window_selection()?;
    if !selection.is_collapsed() {
        return None;
    }
    let node = selection.anchor_node()?;
    if node.node_type() != Node::TEXT_NODE || !host.contains(Some(&node)) {
        return None;
    }
    Some((node, selection.anchor_offset()))
}

/// The text before the caret. DOM offsets count UTF-16 code units.
fn prefix_utf16(text: &str, offset: u32) -> String {
    let units: Vec<u16> = text.encode_utf16().take(offset as usize).collect();
    String::from_utf16_lossy(&units)
}

fn trailing_run(text: &str) -> String {
    let run: String = text
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '.')
        .collect();
    run.chars().rev().collect()
}

/// A `\name` or a directly typed glyph such as `√`, with how much text it
/// takes with it. Names that are just a glyph stay text.
fn trailing_shortcut(text: &str) -> Option<(usize, Seed)> {
    use crate::math::ast::Node as MathNode;
    if let Some(name) = trailing_command(text) {
        if let Some(node) = commands::node_for(&name) {
            let consumed = name.encode_utf16().count() + 1;
            let seed = match node {
                MathNode::Sym(name) => Seed::Text(commands::glyph_for(&name)?.to_string()),
                MathNode::Func(name) => Seed::Text(name),
                node => Seed::Node(node),
            };
            return Some((consumed, seed));
        }
    }
    let glyph = text.chars().next_back()?;
    let node = commands::node_for_glyph(glyph)?;
    Some((glyph.len_utf16(), Seed::Node(node)))
}

fn trailing_command(text: &str) -> Option<String> {
    let letters: String = text
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect();
    if letters.is_empty() {
        return None;
    }
    let name: String = letters.chars().rev().collect();
    let start = text.len() - name.len();
    text[..start].ends_with('\\').then_some(name)
}

fn next_element(node: &Node) -> Option<Element> {
    let mut current = node.clone();
    loop {
        if let Some(sibling) = current.next_sibling() {
            return sibling.dyn_into::<Element>().ok();
        }
        current = current.parent_node()?;
    }
}

fn previous_element(node: &Node) -> Option<Element> {
    let mut current = node.clone();
    loop {
        if let Some(sibling) = current.previous_sibling() {
            return sibling.dyn_into::<Element>().ok();
        }
        current = current.parent_node()?;
    }
}

fn window_selection() -> Option<Selection> {
    web_sys::window()?.get_selection().ok().flatten()
}

/// Attaches behaviour to fields that appeared through undo, paste or loading.
pub fn attach_new_fields() {
    let Some(host) = host() else { return };
    let Ok(nodes) = host.query_selector_all(&format!(".{FIELD_CLASS}")) else {
        return;
    };
    for i in 0..nodes.length() {
        if let Some(element) = nodes.item(i).and_then(|n| n.dyn_into::<HtmlElement>().ok()) {
            field::attach(&element);
        }
    }
}

/// Inserts plain text at the caret, for symbols that need no formula.
pub fn insert_text(text: &str) {
    let (Some(host), Some(doc)) = (host(), document()) else {
        return;
    };
    host.focus().ok();
    let range = caret_range(&host);
    range.delete_contents().ok();
    let inserted = doc.create_text_node(text);
    if range.insert_node(&inserted).is_err() {
        host.append_child(&inserted).ok();
    }
    if let Some(selection) = window_selection() {
        selection
            .collapse_with_offset(Some(&inserted), text.encode_utf16().count() as u32)
            .ok();
    }
    host.normalize();
    notify_change();
}

/// Inserts an empty formula at the caret and starts editing it.
pub fn insert_math(display: bool) {
    let (Some(host), Some(doc)) = (host(), document()) else {
        return;
    };
    host.focus().ok();
    let element = field::create_element(&doc, "", display);
    let range = caret_range(&host);
    range.delete_contents().ok();
    if range.insert_node(&element).is_err() {
        host.append_child(&element).ok();
    }
    let Ok(element) = element.dyn_into::<HtmlElement>() else {
        return;
    };
    field::attach(&element);
    field::focus_at_end(&element);
    notify_change();
}

/// The current caret position, or the end of the document when the caret is
/// somewhere else entirely.
fn caret_range(host: &HtmlElement) -> Range {
    if let Some(selection) = window_selection() {
        if selection.range_count() > 0 {
            if let Ok(range) = selection.get_range_at(0) {
                if let Ok(container) = range.start_container() {
                    if host.contains(Some(&container)) {
                        return range;
                    }
                }
            }
        }
    }
    let doc = host.owner_document().expect("document");
    let range = doc.create_range().expect("range");
    range.select_node_contents(host).ok();
    range.collapse_with_to_start(false);
    range
}

/// Puts the caret back into the text after editing a formula.
pub fn leave_field(field_host: &HtmlElement, escape: Escape) {
    let (Some(host), Some(doc)) = (host(), document()) else {
        return;
    };
    let Ok(range) = doc.create_range() else {
        return;
    };
    match escape {
        Escape::Left => {
            range.set_start_before(field_host).ok();
        }
        Escape::Right | Escape::Done => {
            range.set_start_after(field_host).ok();
        }
        Escape::Delete => {
            range.set_start_before(field_host).ok();
            field_host.remove();
        }
    }
    range.collapse_with_to_start(true);
    if let Some(selection) = window_selection() {
        selection.remove_all_ranges().ok();
        selection.add_range(&range).ok();
    }
    host.focus().ok();
    notify_change();
}

/// Serialises the document to Markdown with formulas as `$...$` / `$$...$$`.
pub fn to_markdown() -> String {
    let Some(host) = host() else {
        return String::new();
    };
    let mut sink = Sink {
        lines: vec![String::new()],
    };
    let children = host.child_nodes();
    for i in 0..children.length() {
        if let Some(node) = children.item(i) {
            sink.walk(&node);
        }
    }
    while sink.lines.last().is_some_and(|line| line.is_empty()) && sink.lines.len() > 1 {
        sink.lines.pop();
    }
    sink.lines.join("\n")
}

/// Serialises the document to standalone HTML with MathML formulas.
pub fn to_html(title: &str) -> String {
    markdown::to_html(&to_markdown(), title)
}

pub fn stats() -> (usize, usize) {
    let text = to_markdown();
    let lines = text.lines().count().max(1);
    let characters = text.chars().filter(|c| *c != '\n').count();
    (characters, lines)
}

struct Sink {
    lines: Vec<String>,
}

impl Sink {
    fn push_text(&mut self, text: &str) {
        for (i, part) in text.split('\n').enumerate() {
            if i > 0 {
                self.newline();
            }
            if let Some(last) = self.lines.last_mut() {
                last.push_str(part);
            }
        }
    }

    fn newline(&mut self) {
        self.lines.push(String::new());
    }

    fn ensure_fresh_line(&mut self) {
        if !self.lines.last().is_some_and(|line| line.is_empty()) {
            self.newline();
        }
    }

    fn walk(&mut self, node: &Node) {
        match node.node_type() {
            Node::TEXT_NODE => {
                let text = node.text_content().unwrap_or_default();
                self.push_text(&markdown::escape_text(&text));
            }
            Node::ELEMENT_NODE => {
                let Ok(element) = node.clone().dyn_into::<Element>() else {
                    return;
                };
                let tag = element.tag_name().to_uppercase();
                if element.class_list().contains(FIELD_CLASS) {
                    let latex = field::latex_of(&element);
                    if field::is_display(&element) {
                        self.ensure_fresh_line();
                        self.push_text(&format!("$${latex}$$"));
                        self.newline();
                    } else {
                        self.push_text(&format!("${latex}$"));
                    }
                    return;
                }
                if tag == "BR" {
                    self.newline();
                    return;
                }
                let block = matches!(
                    tag.as_str(),
                    "DIV" | "P" | "LI" | "UL" | "OL" | "BLOCKQUOTE"
                );
                if block {
                    self.ensure_fresh_line();
                }
                let children = element.child_nodes();
                for i in 0..children.length() {
                    if let Some(child) = children.item(i) {
                        self.walk(&child);
                    }
                }
                if block {
                    self.ensure_fresh_line();
                }
            }
            _ => {}
        }
    }
}

/// Replaces the document with the given Markdown.
pub fn load(text: &str) {
    let (Some(host), Some(doc)) = (host(), document()) else {
        return;
    };
    host.set_inner_html("");
    for line in markdown::parse(text) {
        let block = doc.create_element("div").expect("create line");
        if line.is_empty() {
            block
                .append_child(&doc.create_element("br").expect("create br"))
                .ok();
        }
        for segment in line {
            match segment {
                Segment::Text(text) => {
                    block.append_child(&doc.create_text_node(&text)).ok();
                }
                Segment::Math { latex, display } => {
                    block
                        .append_child(&field::create_element(&doc, &latex, display))
                        .ok();
                }
            }
        }
        host.append_child(&block).ok();
    }
    attach_new_fields();
    // Opening a document starts a fresh history; restoring keeps its own.
    if !restoring() {
        reset_history(to_markdown());
    }
}

/// How the text is matched: literally or as a regular expression, and whether
/// case matters.
#[derive(Clone, Copy, Default)]
pub struct SearchOptions {
    pub regex: bool,
    pub case_sensitive: bool,
}

/// Compiles the query, returning nothing when a regular expression is invalid
/// so a half-typed pattern simply finds nothing.
fn compile(query: &str, options: SearchOptions) -> Option<RegExp> {
    if query.is_empty() {
        return None;
    }
    let source = if options.regex {
        query.to_string()
    } else {
        escape_regex(query)
    };
    let flags = if options.case_sensitive { "g" } else { "gi" };
    // `new RegExp` throws on a bad pattern, which Rust cannot catch.
    let make = js_sys::Function::new_with_args(
        "source, flags",
        "try { return new RegExp(source, flags); } catch (e) { return null; }",
    );
    make.call2(&JsValue::NULL, &source.into(), &flags.into())
        .ok()?
        .dyn_into::<RegExp>()
        .ok()
}

fn escape_regex(query: &str) -> String {
    let mut out = String::with_capacity(query.len());
    for c in query.chars() {
        if "\\^$.|?*+()[]{}".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Selects the next occurrence of `query`, wrapping around at the end.
pub fn find_next(query: &str, options: SearchOptions) -> bool {
    let Some(host) = host() else { return false };
    let Some(regex) = compile(query, options) else {
        return false;
    };
    let nodes = text_nodes(&host);
    // Positions are counted in UTF-16 units, the units the DOM itself uses.
    let mut haystack = String::new();
    let mut units: Vec<(Node, u32)> = Vec::new();
    for node in &nodes {
        let text = node.text_content().unwrap_or_default();
        for unit in 0..text.encode_utf16().count() {
            units.push((node.clone(), unit as u32));
        }
        haystack.push_str(&text);
    }
    let start = selection_offset(&nodes).unwrap_or(0);
    let hit = search_from(&regex, &haystack, start as u32).or_else(|| {
        // Wrap around, so repeated searches cycle through the document.
        search_from(&regex, &haystack, 0)
    });
    let Some((from, to)) = hit else { return false };
    let Some((start_node, start_offset)) = units.get(from as usize).cloned() else {
        return false;
    };
    let (end_node, end_offset) = match units.get(to as usize).cloned() {
        Some(position) => position,
        None => match units.last().cloned() {
            Some((node, offset)) => (node, offset + 1),
            None => return false,
        },
    };
    select(&start_node, start_offset, &end_node, end_offset)
}

/// The next match at or after `from`, as UTF-16 start and end positions.
fn search_from(regex: &RegExp, haystack: &str, from: u32) -> Option<(u32, u32)> {
    regex.set_last_index(from);
    let matched = regex.exec(haystack)?;
    let length = matched
        .get(0)
        .as_string()
        .map(|text| text.encode_utf16().count() as u32)?;
    if length == 0 {
        return None;
    }
    let end = regex.last_index();
    Some((end - length, end))
}

/// Replaces every occurrence of `query`, returning how many were replaced.
/// A regular expression may refer back to its groups with `$1`.
pub fn replace_all(query: &str, replacement: &str, options: SearchOptions) -> usize {
    let Some(host) = host() else { return 0 };
    let Some(regex) = compile(query, options) else {
        return 0;
    };
    let mut count = 0;
    for node in text_nodes(&host) {
        let text = node.text_content().unwrap_or_default();
        let hits = count_matches(&regex, &text);
        if hits == 0 {
            continue;
        }
        count += hits;
        regex.set_last_index(0);
        let replaced =
            js_sys::JsString::from(text.as_str()).replace_by_pattern(&regex, replacement);
        node.set_text_content(Some(&String::from(replaced)));
    }
    if count > 0 {
        notify_change();
    }
    count
}

fn count_matches(regex: &RegExp, text: &str) -> usize {
    regex.set_last_index(0);
    let mut count = 0;
    let mut previous = 0;
    while regex.exec(text).is_some() {
        let index = regex.last_index();
        // An empty match never advances, so stop instead of spinning.
        if index <= previous && count > 0 {
            break;
        }
        previous = index;
        count += 1;
        if index as usize >= text.encode_utf16().count() {
            break;
        }
    }
    count
}

fn text_nodes(host: &HtmlElement) -> Vec<Node> {
    let mut nodes = Vec::new();
    collect_text_nodes(host.as_ref(), &mut nodes);
    nodes
}

fn collect_text_nodes(node: &Node, out: &mut Vec<Node>) {
    let children = node.child_nodes();
    for i in 0..children.length() {
        let Some(child) = children.item(i) else {
            continue;
        };
        match child.node_type() {
            Node::TEXT_NODE => out.push(child),
            Node::ELEMENT_NODE => {
                let is_field = child
                    .clone()
                    .dyn_into::<Element>()
                    .map(|element| element.class_list().contains(FIELD_CLASS))
                    .unwrap_or(false);
                if !is_field {
                    collect_text_nodes(&child, out);
                }
            }
            _ => {}
        }
    }
}

/// A caret position the document can be restored to: a place in the text with
/// every formula counted as a single unit.
struct Stop {
    units: usize,
    node: Node,
    offset: u32,
}

fn caret_stops(host: &HtmlElement) -> Vec<Stop> {
    let mut stops = Vec::new();
    let mut units = 0;
    let children = host.child_nodes();
    for i in 0..children.length() {
        let Some(child) = children.item(i) else {
            continue;
        };
        // The line breaks between blocks are positions of their own.
        if i > 0 && child.node_type() == Node::ELEMENT_NODE && !is_field(&child) {
            stops.push(Stop {
                units,
                node: host.clone().into(),
                offset: i,
            });
            units += 1;
        }
        collect_stops(&child, &mut units, &mut stops);
    }
    stops.push(Stop {
        units,
        node: host.clone().into(),
        offset: children.length(),
    });
    stops
}

fn collect_stops(node: &Node, units: &mut usize, stops: &mut Vec<Stop>) {
    match node.node_type() {
        Node::TEXT_NODE => {
            let len = node
                .text_content()
                .unwrap_or_default()
                .encode_utf16()
                .count();
            for offset in 0..=len {
                stops.push(Stop {
                    units: *units + offset,
                    node: node.clone(),
                    offset: offset as u32,
                });
            }
            *units += len;
        }
        Node::ELEMENT_NODE => {
            let tag = node
                .clone()
                .dyn_into::<Element>()
                .map(|element| element.tag_name())
                .unwrap_or_default();
            if is_field(node) || tag == "BR" {
                if let (Some(parent), Some(index)) = (node.parent_node(), child_index(node)) {
                    stops.push(Stop {
                        units: *units,
                        node: parent.clone(),
                        offset: index,
                    });
                    *units += 1;
                    stops.push(Stop {
                        units: *units,
                        node: parent,
                        offset: index + 1,
                    });
                }
                return;
            }
            let children = node.child_nodes();
            for i in 0..children.length() {
                if let Some(child) = children.item(i) {
                    collect_stops(&child, units, stops);
                }
            }
        }
        _ => {}
    }
}

fn child_index(node: &Node) -> Option<u32> {
    let parent = node.parent_node()?;
    let children = parent.child_nodes();
    (0..children.length()).find(|&i| {
        children
            .item(i)
            .is_some_and(|child| child.is_same_node(Some(node)))
    })
}

fn is_field(node: &Node) -> bool {
    node.clone()
        .dyn_into::<Element>()
        .map(|element| element.class_list().contains(FIELD_CLASS))
        .unwrap_or(false)
}

fn caret_units() -> Option<usize> {
    let host = host()?;
    let selection = window_selection()?;
    let focus = selection.focus_node()?;
    caret_stops(&host)
        .into_iter()
        .find(|stop| {
            stop.node.is_same_node(Some(&focus)) && stop.offset == selection.focus_offset()
        })
        .map(|stop| stop.units)
}

fn place_caret(units: usize) {
    let (Some(host), Some(selection)) = (host(), window_selection()) else {
        return;
    };
    let stops = caret_stops(&host);
    let stop = stops
        .iter()
        .find(|stop| stop.units == units)
        .or_else(|| stops.last());
    if let Some(stop) = stop {
        host.focus().ok();
        selection
            .collapse_with_offset(Some(&stop.node), stop.offset)
            .ok();
    }
}

fn selection_offset(nodes: &[Node]) -> Option<usize> {
    let selection = window_selection()?;
    let focus = selection.focus_node()?;
    let mut total = 0;
    for node in nodes {
        if node.is_same_node(Some(&focus)) {
            return Some(total + selection.focus_offset() as usize);
        }
        total += node
            .text_content()
            .unwrap_or_default()
            .encode_utf16()
            .count();
    }
    None
}

fn select(start: &Node, start_offset: u32, end: &Node, end_offset: u32) -> bool {
    let Some(doc) = document() else { return false };
    let Ok(range) = doc.create_range() else {
        return false;
    };
    if range.set_start(start, start_offset).is_err() || range.set_end(end, end_offset).is_err() {
        return false;
    }
    let Some(selection) = window_selection() else {
        return false;
    };
    selection.remove_all_ranges().ok();
    selection.add_range(&range).ok();
    if let Some(element) = start.parent_element() {
        element.scroll_into_view_with_bool(false);
    }
    true
}
