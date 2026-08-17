//! bi — a batteries-included modal editor, as a library.
//!
//! This crate is the editor: text, history, motions, the keymap, registers and
//! the parse tree. It knows nothing about terminals. Rendering and event input
//! live in a frontend — `src/tui/` is the one that ships, and a GUI or an
//! embedding would sit in the same place.
//!
//! The boundary is a convention, not a compiler rule: a lib and a bin in one
//! package share one dependency list, so nothing stops a module here from
//! reaching for a terminal library. The rule is that no module declared below
//! may name one, and `tests/lib_boundary.rs` fails the build if one does. Only
//! splitting into a workspace would make that a compiler rule, and a workspace
//! is not worth its churn for a single frontend.
//!
//! The seams the remaining work needs already exist: [`buffer::Edit`] and the
//! [`editor::Action`] table, plus [`editor::Editor::settle`], which drains
//! the edit log for tree-sitter today and for LSP `didChange` later.

pub mod buffer;
pub mod config;
pub mod editor;
pub mod history;
pub mod input;
pub mod key;
pub mod motion;
pub mod picker;
pub mod registers;
pub mod selection;
pub mod syntax;
pub mod tree;
pub mod window;
