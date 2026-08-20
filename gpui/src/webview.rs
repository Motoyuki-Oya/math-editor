//! GPUI ウィンドウ内で Wry WebView を保持し、レンダリング時に位置を同期します。

use gpui::*;
use std::rc::Rc;
use wry::{
    dpi::{LogicalPosition, LogicalSize, Position, Size as WrySize},
    Rect, WebView as WryWebView, WebViewBuilder,
};

use crate::bridge::{IpcHandler, INIT_SCRIPT};
use crate::protocol::{asset_response, url};

pub struct Webview {
    focus_handle: FocusHandle,
    view: Rc<WryWebView>,
}

impl Webview {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        use wry::raw_window_handle::HasWindowHandle;

        let focus_handle = cx.focus_handle();
        let window_handle = HasWindowHandle::window_handle(window).expect("no window handle");

        let view: Rc<WryWebView> = Rc::new_cyclic(|weak| {
            let builder = WebViewBuilder::new()
                .with_url(url())
                .with_initialization_script(INIT_SCRIPT)
                .with_custom_protocol("planetext".to_string(), |_id, req| asset_response(_id, req))
                .with_ipc_handler({
                    let weak = weak.clone();
                    move |req| {
                        if let Some(view) = weak.upgrade() {
                            IpcHandler::new(view).handle(req);
                        }
                    }
                })
                .with_bounds(Rect {
                    size: WrySize::Logical(LogicalSize::new(1200.0, 800.0)),
                    position: Position::Logical(LogicalPosition::new(0.0, 0.0)),
                });

            builder
                .build_as_child(&window_handle)
                .expect("failed to build webview")
        });

        Self { focus_handle, view }
    }
}

impl Render for Webview {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let view = self.view.clone();
        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .child(WebviewElement::new(view))
    }
}

struct WebviewElement {
    view: Rc<WryWebView>,
    bounds: Option<Bounds<Pixels>>,
}

impl WebviewElement {
    fn new(view: Rc<WryWebView>) -> Self {
        Self { view, bounds: None }
    }
}

impl IntoElement for WebviewElement {
    type Element = WebviewElement;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for WebviewElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        _cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let style = Style {
            size: Size::full(),
            ..Default::default()
        };
        (window.request_layout(style, [], _cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
        if self.bounds != Some(bounds) {
            self.bounds = Some(bounds);
            let _ = self.view.set_bounds(Rect {
                size: WrySize::Logical(LogicalSize::new(
                    to_f64(bounds.size.width),
                    to_f64(bounds.size.height),
                )),
                position: Position::Logical(LogicalPosition::new(
                    to_f64(bounds.origin.x),
                    to_f64(bounds.origin.y),
                )),
            });
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        _window: &mut Window,
        _cx: &mut App,
    ) {
    }
}

fn to_f64(p: Pixels) -> f64 {
    f64::from(f32::from(p))
}
