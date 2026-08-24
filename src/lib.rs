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
//! the edit log for tree-sitter and for LSP `didChange` alike.

pub mod alternate;
pub mod buffer;
pub mod case;
pub mod clipboard;
pub mod cmd_history;
pub mod cmdline;
pub mod colors;
pub mod complete;
pub mod config;
pub mod context;
pub mod decoration;
pub mod editor;
pub mod editorconfig;
pub mod files;
pub mod find_in_files;
pub mod git;
pub mod gitignore;
pub mod history;
pub mod indent;
pub mod input;
pub mod key;
pub mod label;
pub mod lsp;
pub mod motion;
pub mod picker;
pub mod range;
pub mod reflow;
pub mod region;
pub mod registers;
pub mod resize;
pub mod results;
pub mod selection;
pub mod sort;
pub mod substitute;
pub mod surround;
pub mod syntax;
pub mod theme;
pub mod todo;
pub mod tree;
pub mod trim;
pub mod window;
