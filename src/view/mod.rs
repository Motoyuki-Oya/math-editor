//! How a document is drawn.
//!
//! Depends on [`crate::structure`] only. It must never reach into
//! [`crate::format`]: nothing on screen may be derived from the notation, which
//! is why the palette, the tooltips and the caret all work in terms of
//! structures instead.
//!
//! [`row`] is the one component that draws a row, at every depth; [`document`]
//! puts the lines together and measures what it drew.

pub mod document;
pub mod row;
