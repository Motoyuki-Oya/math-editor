pub(crate) fn set_clipboard_text(text: String) -> Result<(), String> {
    arboard::Clipboard::new()
        .and_then(|mut clipboard| clipboard.set_text(text))
        .map_err(|e| format!("コピーできませんでした: {e}"))
}
