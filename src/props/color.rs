//! Colours as the format spells them: `#rrggbb` and `#rrggbbaa` — the
//! picker's hex, re-exported where the format reads it. See
//! `docs/specs/bi-format.md`.

pub use crate::color_picker::{hex as text, parse_hex as parse};
