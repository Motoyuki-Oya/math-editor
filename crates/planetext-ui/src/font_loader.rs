//! フォントのオンデマンドダウンロードとローカル保存（localStorage キャッシュ）

use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Document, Response};

pub const DOWNLOADABLE_FONTS: &[(&str, &str)] = &[
    (
        "Fira Code",
        "https://cdn.jsdelivr.net/npm/firacode@6.2.0/distr/woff2/FiraCode-Regular.woff2",
    ),
    (
        "JetBrains Mono",
        "https://cdn.jsdelivr.net/npm/jetbrains-mono@1.0.6/fonts/webfonts/JetBrainsMono-Regular.woff2",
    ),
    (
        "Source Code Pro",
        "https://cdn.jsdelivr.net/npm/source-code-pro@2.38.0/WOFF2/TTF/SourceCodePro-Regular.ttf.woff2",
    ),
];

pub fn get_download_url(font_name: &str) -> Option<&'static str> {
    DOWNLOADABLE_FONTS
        .iter()
        .find(|(name, _)| *name == font_name)
        .map(|(_, url)| *url)
}

/// localStorage から保存済みフォントを読み込み、<head> に @font-face スタイルを注入します。
pub fn load_saved_fonts() {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Ok(Some(storage)) = window.local_storage() else {
        return;
    };
    let Some(document) = window.document() else {
        return;
    };
    for (font_name, _) in DOWNLOADABLE_FONTS {
        let key = format!("planetext_font_{font_name}");
        if let Ok(Some(base64_data)) = storage.get_item(&key) {
            inject_font_face(&document, font_name, &base64_data);
        }
    }
}

/// <head> に @font-face スタイル要素を追加または更新します。
pub fn inject_font_face(document: &Document, font_name: &str, base64_data: &str) {
    let style_id = format!("planetext-font-style-{}", font_name.replace(' ', "-"));
    if let Some(existing) = document.get_element_by_id(&style_id) {
        existing.set_text_content(Some(&format!(
            r#"@font-face {{ font-family: "{font_name}"; src: local("{font_name}"), url("data:font/woff2;base64,{base64_data}") format("woff2"); font-weight: 400; font-style: normal; font-display: swap; }}"#
        )));
        return;
    }
    if let Ok(style) = document.create_element("style") {
        style.set_id(&style_id);
        style.set_text_content(Some(&format!(
            r#"@font-face {{ font-family: "{font_name}"; src: local("{font_name}"), url("data:font/woff2;base64,{base64_data}") format("woff2"); font-weight: 400; font-style: normal; font-display: swap; }}"#
        )));
        if let Some(head) = document.head() {
            head.append_child(&style).ok();
        }
    }
}

/// フォントが保存済み、またはローカルで利用可能かを判定します。
pub fn is_font_cached_or_local(font_name: &str) -> bool {
    let Some(window) = web_sys::window() else {
        return true;
    };
    if let Ok(Some(storage)) = window.local_storage() {
        let key = format!("planetext_font_{font_name}");
        if storage.get_item(&key).ok().flatten().is_some() {
            return true;
        }
    }
    // Cascadia Code / Meiryo / Segoe UI / Consolas / BIZ UDゴシック はOS標準
    if matches!(
        font_name,
        "Cascadia Code" | "Meiryo" | "Segoe UI" | "Consolas" | "BIZ UDゴシック" | ""
    ) {
        return true;
    }
    false
}

fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let triple = ((b0 as u32) << 16) | ((b1 as u32) << 8) | (b2 as u32);
        out.push(TABLE[((triple >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// フォントをダウンロードしてローカル（localStorage）に保存し、即時適用します。
pub async fn download_and_save_font(font_name: &str, url: &str) -> Result<(), String> {
    let Some(window) = web_sys::window() else {
        return Err("No window".into());
    };
    let resp_value = JsFuture::from(window.fetch_with_str(url))
        .await
        .map_err(|e| format!("{e:?}"))?;
    let resp: Response = resp_value.dyn_into().map_err(|e| format!("{e:?}"))?;
    let buffer_value = JsFuture::from(resp.array_buffer().map_err(|e| format!("{e:?}"))?)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let uint8_array = js_sys::Uint8Array::new(&buffer_value);
    let bytes = uint8_array.to_vec();

    let base64_str = base64_encode(&bytes);

    if let Ok(Some(storage)) = window.local_storage() {
        let key = format!("planetext_font_{font_name}");
        storage.set_item(&key, &base64_str).ok();
    }
    if let Some(document) = window.document() {
        inject_font_face(&document, font_name, &base64_str);
    }
    Ok(())
}
