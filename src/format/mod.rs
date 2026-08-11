//! How a document is written to a file, and read back.
//!
//! Depends on [`crate::structure`] only. It must never reach into
//! [`crate::view`], and nothing here may touch the DOM: the notation has to be
//! readable and writable without a screen.

pub mod document;
pub mod islands;
pub mod notation;
