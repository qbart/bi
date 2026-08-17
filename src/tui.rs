//! The terminal frontend.
//!
//! Everything that knows what a terminal is lives under here: ratatui drawing
//! in [`render`], and crossterm event translation in [`keys`]. The `bi` library
//! is frontend-agnostic and must stay that way — a GUI would be a sibling of
//! this module, not a rewrite of the core.

pub mod keys;
pub mod render;
