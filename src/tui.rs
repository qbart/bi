//! The terminal frontend.
//!
//! Everything that knows what a terminal is lives under here: ratatui drawing
//! in [`render`], crossterm event translation in [`keys`], and the OSC 52
//! clipboard in [`clipboard`]. The `bi` library
//! is frontend-agnostic and must stay that way — a GUI would be a sibling of
//! this module, not a rewrite of the core.

pub mod clipboard;
pub mod graphics;
pub mod keys;
pub mod render;
