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
/// Four variants rather than an anchor and a payload, because every
/// combination of those that would be legal is one of these four, and the
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
    /// Draw `text` *between* the cells at (`row`, `col`), pushing the rest of
    /// the row right by its width.
    ///
    /// What an [`Decoration::Overlay`] cannot do: a jump label has to be
    /// readable *and* leave the character it points at readable, and one cell
    /// cannot hold both. The row is wider than the text for as long as the
    /// letters are up, which is the price and is worth it — a label that hides
    /// the thing you are aiming at is aiming for you.
    ///
    /// Two of these at the same column land in the order they were produced,
    /// left to right, so a place two labels want is two cells rather than an
    /// argument.
    ///
    /// Always painted last, over everything: same reason as `Eol`, and so
    /// there is no `layer` to choose.
    Inline { row: usize, col: usize, text: String, style: Style },
    /// Draw `text` after the end of `row`, past whatever is there.
    Eol { row: usize, text: String, style: Style },
}

impl Decoration {
    pub fn layer(&self) -> Layer {
        match self {
            Decoration::Repaint { layer, .. } | Decoration::Overlay { layer, .. } => *layer,
            // Neither of these is *on* the text to be over or under it: one is
            // out past the end of the line and the other makes its own cells.
            Decoration::Eol { .. } | Decoration::Inline { .. } => Layer::Over,
        }
    }

    pub fn style(&self) -> Style {
        match self {
            Decoration::Repaint { style, .. }
            | Decoration::Overlay { style, .. }
            | Decoration::Inline { style, .. }
            | Decoration::Eol { style, .. } => *style,
        }
    }
}
