//! Planetext の GPUI + WebView (Wry) 版。
//! 既存の Leptos/Tauri フロントエンドを GPUI ウィンドウ内の Wry WebView で動かし、
//! Tauri の `__TAURI__.core.invoke` の挙動をエミュレートしてバックエンド機能を提供します。

mod bridge;
mod protocol;
mod webview;

use gpui::*;
use webview::Webview;

fn main() {
    Application::new().run(|cx: &mut App| {
        #[cfg(target_os = "linux")]
        {
            gtk::init().expect("gtk init failed");
        }

        let bounds = Bounds::centered(None, size(px(1200.0), px(800.0)), cx);
        cx.activate(true);

        let _ = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    kind: WindowKind::Normal,
                    ..Default::default()
                },
                |window, cx| cx.new(|cx| Webview::new(window, cx)),
            )
            .unwrap();
    });
}
