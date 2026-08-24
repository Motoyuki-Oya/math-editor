//! エディター コア: ブラウザーに「contenteditable」要素を編集させる代わりに、エディター自体がレンダリングするドキュメント。キーボードと IME の入力は、隠しテキストエリアを介して届きます。これにより、複数のカーソルが可能になります。

pub mod clipboard;
mod commands;
mod input;
mod keys;
pub mod model;
mod mouse;
pub mod search;
mod session;
mod trigger;

pub use commands::{
    annotate, apply_far_match, far_search_start, find_far_in_line, find_next, insert_node,
    replace_all, select_all, FarCopy,
};
pub use search::SearchOptions;
pub use session::{
    apply_restored, clear_search_preview, close_pane, feed_pane, focus_pane, fully_resident, init,
    load, load_pending, park, preview_search, redraw_all, restore, set_line_count, set_on_change,
    set_on_far_copy, set_on_missing, stats, take_flush, FlushBatch, Parked,
};
