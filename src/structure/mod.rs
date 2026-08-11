//! What the text means: ordinary characters plus the two dimensional
//! structures that characters alone cannot hold.
//!
//! This layer is the only thing [`crate::format`] and [`crate::view`] share, so
//! it must not depend on either of them: it knows nothing about how a structure
//! is written to a file, and nothing about how it is drawn. Keeping it that way
//! is what lets the notation and the display change without touching each
//! other (see `docs/architecture.md`).

pub mod ast;
pub mod commands;
pub mod edit;
pub mod symbols;
pub mod text;
