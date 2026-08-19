//! What is drawn that is not buffer text.
//!
//! Indent guides, `TODO:` picked out of a comment, `#ffaacc` in the colour it
//! names, the letters you jump by, a flash on what was yanked, and inline
//! diagnostics when there is an LSP to have them: seven features of one shape,
//! none of them the frontend's business. They go through this.
//!
//! Producing them is [`crate::editor::Editor::decorations`]; painting them is
//! the frontend. See `docs/specs/decorations.md`.

use std::ops::Range;

use crate::theme::Style;

/// Whether a decoration goes under the selection or over it.
///
/// Two values because two is what the clients need. Guides, swatches and
/// comment tags belong under: selecting a line has to look like selecting a
/// line. A jump label belongs over: a letter you are about to press has to be
/// readable wherever it lands. A z-order integer would be a number nobody
/// could choose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    Under,
    Over,
}

/// One thing to draw.
///
/// Three variants rather than an anchor and a payload, because every
/// combination of those that would be legal is one of these three, and the
/// rest are nonsense a type should not be able to say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decoration {
    /// Repaint the cells a char range already occupies. The text is unchanged.
    ///
    /// Char offsets, because a `TODO:` is a range of *text* and has to follow
    /// it as the line is edited.
    Repaint { range: Range<usize>, style: Style, layer: Layer },
    /// Draw `text` over the cells at (`row`, `col`), replacing what is there
    /// and moving nothing.
    ///
    /// Display columns, because a guide at column 4 of a tab-indented line
    /// sits *inside* a tab and has no char offset at all. `col` is a column of
    /// the text area — the frontend adds its own gutter.
    Overlay { row: usize, col: usize, text: String, style: Style, layer: Layer },
    /// Draw `text` after the end of `row`, past whatever is there.
    Eol { row: usize, text: String, style: Style },
}

impl Decoration {
    pub fn layer(&self) -> Layer {
        match self {
            Decoration::Repaint { layer, .. } | Decoration::Overlay { layer, .. } => *layer,
            // Nothing else is out past the end of the line to be over or
            // under.
            Decoration::Eol { .. } => Layer::Over,
        }
    }

    pub fn style(&self) -> Style {
        match self {
            Decoration::Repaint { style, .. }
            | Decoration::Overlay { style, .. }
            | Decoration::Eol { style, .. } => *style,
        }
    }
}
