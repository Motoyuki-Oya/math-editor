//! The document surface: ordinary text editing handled by the browser, with
//! formulas embedded as islands the browser does not touch.

use std::cell::RefCell;

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
/// into math without a menu, the way Markdown shortcuts work.
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
            Some((consumed, node)) => (consumed, Seed::Node(node)),
            None => return,
        },
        _ => return,
    };

    event.prevent_default();
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
/// takes with it when it turns into a formula.
fn trailing_shortcut(text: &str) -> Option<(usize, crate::math::ast::Node)> {
    if let Some(name) = trailing_command(text) {
        if let Some(node) = commands::node_for(&name) {
            return Some((name.encode_utf16().count() + 1, node));
        }
    }
    let glyph = text.chars().next_back()?;
    let node = commands::node_for_glyph(glyph)?;
    Some((glyph.len_utf16(), node))
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
}

/// Selects the next occurrence of `query`, wrapping around at the end.
pub fn find_next(query: &str) -> bool {
    if query.is_empty() {
        return false;
    }
    let Some(host) = host() else { return false };
    let nodes = text_nodes(&host);
    let needle = query.to_lowercase();
    let start = selection_offset(&nodes).unwrap_or(0);
    let mut haystack = String::new();
    let mut offsets: Vec<(usize, Node, usize)> = Vec::new();
    for node in &nodes {
        let text = node.text_content().unwrap_or_default();
        for (index, _) in text.char_indices() {
            offsets.push((haystack.len() + index, node.clone(), index));
        }
        haystack.push_str(&text.to_lowercase());
    }
    let found = haystack[start.min(haystack.len())..]
        .find(&needle)
        .map(|hit| hit + start)
        .or_else(|| haystack.find(&needle));
    let Some(hit) = found else { return false };
    let end = hit + needle.len();
    let Some((_, start_node, start_offset)) =
        offsets.iter().find(|(pos, _, _)| *pos == hit).cloned()
    else {
        return false;
    };
    let (end_node, end_offset) = match offsets.iter().find(|(pos, _, _)| *pos == end).cloned() {
        Some((_, node, offset)) => (node, offset),
        None => {
            let (_, node, offset) = offsets.last().cloned().unwrap();
            (node.clone(), offset + 1)
        }
    };
    select(&start_node, start_offset, &end_node, end_offset)
}

/// Replaces every occurrence of `query`, returning how many were replaced.
pub fn replace_all(query: &str, replacement: &str) -> usize {
    if query.is_empty() {
        return 0;
    }
    let Some(host) = host() else { return 0 };
    let mut count = 0;
    for node in text_nodes(&host) {
        let text = node.text_content().unwrap_or_default();
        let hits = text.to_lowercase().matches(&query.to_lowercase()).count();
        if hits == 0 {
            continue;
        }
        count += hits;
        node.set_text_content(Some(&replace_ignore_case(&text, query, replacement)));
    }
    if count > 0 {
        notify_change();
    }
    count
}

fn replace_ignore_case(text: &str, query: &str, replacement: &str) -> String {
    let lower = text.to_lowercase();
    let needle = query.to_lowercase();
    let mut out = String::new();
    let mut cursor = 0;
    while let Some(hit) = lower[cursor..].find(&needle) {
        let start = cursor + hit;
        out.push_str(&text[cursor..start]);
        out.push_str(replacement);
        cursor = start + needle.len();
    }
    out.push_str(&text[cursor..]);
    out
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

fn selection_offset(nodes: &[Node]) -> Option<usize> {
    let selection = window_selection()?;
    let focus = selection.focus_node()?;
    let mut total = 0;
    for node in nodes {
        if node.is_same_node(Some(&focus)) {
            return Some(total + selection.focus_offset() as usize);
        }
        total += node.text_content().unwrap_or_default().len();
    }
    None
}

fn select(start: &Node, start_offset: usize, end: &Node, end_offset: usize) -> bool {
    let Some(doc) = document() else { return false };
    let Ok(range) = doc.create_range() else {
        return false;
    };
    if range.set_start(start, start_offset as u32).is_err()
        || range.set_end(end, end_offset as u32).is_err()
    {
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
