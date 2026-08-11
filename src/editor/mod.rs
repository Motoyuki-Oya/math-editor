//! The editor core: a document the editor renders itself, instead of letting
//! the browser edit a `contenteditable` element. Keyboard and IME input arrive
//! through a hidden textarea, which is what makes several cursors possible.

mod input;
pub mod model;
pub mod search;
mod state;
mod trigger;

pub use search::SearchOptions;
pub use state::{
    close_pane, find_next, focus_pane, init, insert_math, insert_node, load, park, redo,
    replace_all, restore, set_on_change, stats, to_document, undo, Parked,
};
