//! The editor core: a document the editor renders itself, instead of letting
//! the browser edit a `contenteditable` element. Keyboard and IME input arrive
//! through a hidden textarea, which is what makes several cursors possible.

mod input;
pub mod model;
pub mod search;
mod state;
mod trigger;
pub mod view;

pub use search::SearchOptions;
pub use state::{
    find_next, focus, init, insert_math, insert_node, insert_plain, load, park, redo, replace_all,
    restore, set_on_change, stats, to_document, undo, Parked,
};
