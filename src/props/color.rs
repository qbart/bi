//! Colours as the format spells them: `#rrggbb` and `#rrggbbaa`. See
//! `docs/specs/bi-format.md`.

/// A colour read from its hex text: `#` optional, six digits, or eight
/// when `alpha`; six digits with `alpha` read as opaque. Case is free.
pub fn parse(text: &str, alpha: bool) -> Option<[u8; 4]> {
    let hex = text.trim().strip_prefix('#').unwrap_or(text.trim());
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    match (hex.len(), alpha) {
        (6, _) => Some([byte(0)?, byte(2)?, byte(4)?, 0xff]),
        (8, true) => Some([byte(0)?, byte(2)?, byte(4)?, byte(6)?]),
        _ => None,
    }
}

/// The colour as the editor writes it: lowercase, `#` first, eight
/// digits when `alpha`.
pub fn text(c: [u8; 4], alpha: bool) -> String {
    if alpha {
        format!("#{:02x}{:02x}{:02x}{:02x}", c[0], c[1], c[2], c[3])
    } else {
        format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_reads_and_writes() {
        assert_eq!(parse("#c83c1e", false), Some([0xc8, 0x3c, 0x1e, 0xff]));
        assert_eq!(parse("C83C1E", false), Some([0xc8, 0x3c, 0x1e, 0xff]));
        assert_eq!(parse("#c83c1e80", true), Some([0xc8, 0x3c, 0x1e, 0x80]));
        assert_eq!(
            parse("#c83c1e", true),
            Some([0xc8, 0x3c, 0x1e, 0xff]),
            "six digits read opaque"
        );
        assert_eq!(parse("#c83c1e80", false), None, "no alpha on an rgb");
        assert_eq!(parse("red", false), None);
        assert_eq!(parse("#c83c1", false), None);
        assert_eq!(text([0xc8, 0x3c, 0x1e, 0x80], false), "#c83c1e");
        assert_eq!(text([0xc8, 0x3c, 0x1e, 0x80], true), "#c83c1e80");
    }
}
