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

pub use commands::{find_next, insert_math, insert_node, redo, replace_all, select_all, undo};
pub use search::SearchOptions;
pub use session::{
    close_pane, document_of, focus_pane, init, load, park, redraw_all, restore, set_on_change,
    stats, to_document, Parked,
};
