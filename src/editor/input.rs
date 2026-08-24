//! エディター コアのキーボード、IME、およびマウスの処理。
//!
//! 入力は非表示のテキストエリアを経由します。ブラウザーはキーストロークと IME の構成を提供し、すべての変更がモデルのすべてのキャレットに同時に適用されます。

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::convert::FromWasmAbi;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{
    CompositionEvent, Document, HtmlElement, HtmlTextAreaElement, InputEvent, KeyboardEvent,
    MouseEvent,
};

use super::session::{self, Session};
use super::{commands, keys, mouse};

pub fn build(doc: &Document, root: &HtmlElement) -> Option<HtmlTextAreaElement> {
    let textarea = doc
        .create_element("textarea")
        .ok()?
        .dyn_into::<HtmlTextAreaElement>()
        .ok()?;
    textarea.set_class_name("mn-input");
    textarea.set_attribute("autocapitalize", "off").ok();
    textarea.set_attribute("autocomplete", "off").ok();
    textarea.set_attribute("spellcheck", "false").ok();
    textarea.set_attribute("wrap", "off").ok();
    root.append_child(&textarea).ok()?;
    Some(textarea)
}

/// 隠しの入力欄をキャレットの場所 (文書座標) に置きます。IME の候補窓はこの欄のそばに出るので、変換中の文字の近くに見えます。
pub(super) fn follow_caret(textarea: &HtmlTextAreaElement, rect: crate::view::measure::Box2) {
    let style = format!(
        "left:{}px;top:{}px;height:{}px",
        rect.left,
        rect.top,
        rect.height.max(16.0)
    );
    textarea.set_attribute("style", &style).ok();
}

/// エディターを使用可能にするイベントを結び付けます。
pub fn install(session: &Rc<RefCell<Session>>) {
    let textarea = session.borrow().textarea.clone();
    let root = session.borrow().view.root.clone();

    on(
        &textarea,
        "keydown",
        session,
        |session, event: KeyboardEvent| {
            keys::on_keydown(session, event);
        },
    );
    on(&textarea, "input", session, |session, event: InputEvent| {
        commands::on_input(session, event);
    });
    on(
        &textarea,
        "compositionstart",
        session,
        |session, _: CompositionEvent| {
            session.borrow_mut().composing = true;
            session.borrow_mut().preedit.clear();
            session::redraw(session);
        },
    );
    on(
        &textarea,
        "compositionupdate",
        session,
        |session, event: CompositionEvent| {
            commands::update_composition(session, &event.data().unwrap_or_default());
        },
    );
    on(
        &textarea,
        "compositionend",
        session,
        |session, event: CompositionEvent| {
            session.borrow_mut().composing = false;
            let text = event.data().unwrap_or_default();
            commands::commit_composition(session, &text);
        },
    );
    on(
        &textarea,
        "blur",
        session,
        |session, _: web_sys::FocusEvent| {
            session.borrow_mut().focused = false;
            session::redraw(session);
        },
    );
    on(
        &textarea,
        "focus",
        session,
        |session, _: web_sys::FocusEvent| {
            session.borrow_mut().focused = true;
            session::note_focus(session);
            session::redraw(session);
        },
    );

    on(&root, "mousedown", session, |session, event: MouseEvent| {
        mouse::on_mousedown(session, event);
    });
    on(&root, "mousemove", session, |session, event: MouseEvent| {
        mouse::on_mousemove(session, event);
    });
    on(&root, "dblclick", session, |session, event: MouseEvent| {
        mouse::on_dblclick(session, event);
    });
    // 縦はブラウザーにスクロールさせない。ホイールは窓を行の分だけ動かす。
    on(
        &root,
        "wheel",
        session,
        |session, event: web_sys::WheelEvent| {
            let delta = event.delta_y();
            if delta != 0.0 {
                event.prevent_default();
                // deltaMode 1 は行単位。窓の側は画素で受けるので読み替える。
                let pixels = if event.delta_mode() == 1 {
                    delta * 20.0
                } else {
                    delta
                };
                session::wheel(session, pixels);
            }
        },
    );
    // つまみは文書全体のおおよその割合。動いたら窓をそこへ。
    let scrollbar = session.borrow().view.scrollbar();
    on(
        &scrollbar,
        "scroll",
        session,
        |session, _: web_sys::Event| {
            session::thumb_moved(session);
        },
    );
    // 中身の横スクロールでは、重ね描き（選択やキャレット）を測り直す。
    let scroller = session.borrow().view.scroller();
    on(
        &scroller,
        "scroll",
        session,
        |session, _: web_sys::Event| {
            session::scrolled(session);
        },
    );
    if let Some(window) = web_sys::window() {
        let target: web_sys::EventTarget = window.into();
        on(&target, "mouseup", session, |session, _: MouseEvent| {
            session.borrow_mut().dragging = false;
        });
    }
    on(
        &textarea,
        "paste",
        session,
        |session, event: web_sys::ClipboardEvent| {
            if let Some(data) = event.clipboard_data().and_then(|d| d.get_data("text").ok()) {
                event.prevent_default();
                commands::insert_text(session, &data);
            }
        },
    );
    on(
        &textarea,
        "copy",
        session,
        |session, event: web_sys::ClipboardEvent| {
            copy_selection(session, &event, false);
        },
    );
    on(
        &textarea,
        "cut",
        session,
        |session, event: web_sys::ClipboardEvent| {
            copy_selection(session, &event, true);
        },
    );
}

fn copy_selection(session: &Rc<RefCell<Session>>, event: &web_sys::ClipboardEvent, remove: bool) {
    // まだ届いていない行を含む選択は、文書の本体が組み立ててクリップボードへ
    // 置く。届いていない行は編集できないので、切り取りでも削除は起きない。
    if commands::request_far_copy(session) {
        event.prevent_default();
        return;
    }
    // コピーするものがあるかどうかは、読み取られるテキストについてではなく、選択内容についての質問です。空の構造は何も読み取られません。
    let Some(text) = commands::selected_text(session) else {
        return;
    };
    event.prevent_default();
    if let Some(data) = event.clipboard_data() {
        data.set_data("text/plain", &text).ok();
    }
    if remove {
        commands::delete_selection(session);
    }
}

/// 実行中のみセッションを借用するリスナーを追加します。
fn on<E, T>(
    target: &T,
    name: &str,
    session: &Rc<RefCell<Session>>,
    handler: impl Fn(&Rc<RefCell<Session>>, E) + 'static,
) where
    E: FromWasmAbi + 'static,
    T: AsRef<web_sys::EventTarget>,
{
    let session = session.clone();
    let closure = Closure::<dyn FnMut(E)>::new(move |event: E| handler(&session, event));
    target
        .as_ref()
        .add_event_listener_with_callback(name, closure.as_ref().unchecked_ref())
        .ok();
    closure.forget();
}

/// マウス イベントが唯一のキャレットを移動するのではなく、別のキャレットを要求するかどうか。
pub fn adds_caret(event: &MouseEvent) -> bool {
    event.alt_key()
}
