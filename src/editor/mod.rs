//! The editor core: a document the editor renders itself, instead of letting
//! the browser edit a `contenteditable` element. Keyboard and IME input arrive
//! through a hidden textarea, which is what makes several cursors possible.

pub mod clipboard;
mod commands;
mod input;
mod keys;
pub mod model;
mod mouse;
pub mod search;
mod session;
mod trigger;

pub use commands::{find_next, insert_math, insert_node, redo, replace_all, undo};
pub use search::SearchOptions;
pub use session::{
    close_pane, focus_pane, init, load, park, restore, set_on_change, stats, to_document, Parked,
};
