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

pub use commands::{annotate, find_next, insert_node, replace_all, select_all};
pub use search::SearchOptions;
pub use session::{
    apply_restored, close_pane, feed_pane, focus_pane, init, load, load_pending, park, redraw_all,
    restore, set_on_change, set_on_missing, stats, take_flush, FlushBatch, Parked,
};
