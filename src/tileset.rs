//! A grid of tiles over an image, and a cursor on it.
//!
//! State and pixel arithmetic only, like [`crate::img::Img`] — the image
//! owns the pixels and asks this module to read and write one tile of them.
//! See `docs/specs/tileset.md`.

/// One tile's pixels, RGBA8 — what the session's slot holds between a `yy`
/// on one sheet and a `p` on another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tile {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// The shape of a tile. `Hex` parses so the spelling is reserved, and is
/// refused at `:set` until someone builds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Tile,
    Hex,
}

impl Kind {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "tile" => Some(Self::Tile),
            "hex" => Some(Self::Hex),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tile => "tile",
            Self::Hex => "hex",
        }
    }

    /// Why this kind cannot be set, when it cannot.
    pub fn refusal(self) -> Option<&'static str> {
        match self {
            Self::Tile => None,
            Self::Hex => Some("hex is not built yet"),
        }
    }
}

/// `16x16`, or `16` for a square. Zero is not a size.
pub fn parse_size(s: &str) -> Result<(u32, u32), String> {
    let bad = || format!("not a tile size: {s} (want 16 or 16x16)");
    let (w, h) = match s.split_once(['x', 'X']) {
        Some((w, h)) => (w, h),
        None => (s, s),
    };
    let (w, h) =
        (w.trim().parse::<u32>().map_err(|_| bad())?, h.trim().parse::<u32>().map_err(|_| bad())?);
    if w == 0 || h == 0 {
        return Err(bad());
    }
    Ok((w, h))
}

/// `r` and a direction: how a tile is turned in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Turn {
    /// `rh` — a quarter turn counter-clockwise.
    Left,
    /// `rl` — a quarter turn clockwise.
    Right,
    /// `rj` — half way round.
    Half,
    /// `rx` — mirrored left to right.
    MirrorX,
    /// `rk`, `ry` — mirrored top to bottom, which is the half turn followed
    /// by the left-right mirror.
    MirrorY,
}

impl Turn {
    /// `pixels` of a `w`×`h` tile, turned. A quarter turn of a tile that
    /// is not square would not fit its cell, and is refused.
    fn apply(self, pixels: &[u8], w: u32, h: u32) -> Result<Vec<u8>, String> {
        if matches!(self, Self::Left | Self::Right) && w != h {
            return Err(format!("{w}×{h} does not turn"));
        }
        let at =
            |x: u32, y: u32| &pixels[((y * w + x) * 4) as usize..((y * w + x) * 4 + 4) as usize];
        let mut out = Vec::with_capacity(pixels.len());
        for y in 0..h {
            for x in 0..w {
                // Where the pixel landing at (x, y) comes from.
                let (sx, sy) = match self {
                    Self::Right => (y, h - 1 - x),
                    Self::Left => (w - 1 - y, x),
                    Self::Half => (w - 1 - x, h - 1 - y),
                    Self::MirrorX => (w - 1 - x, y),
                    Self::MirrorY => (x, h - 1 - y),
                };
                out.extend_from_slice(at(sx, sy));
            }
        }
        Ok(out)
    }
}

/// One tile overwritten: where, and both versions of its pixels.
#[derive(Debug, Clone)]
struct Edit {
    at: (u32, u32),
    before: Vec<u8>,
    after: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct Tileset {
    /// One tile, in pixels.
    size: (u32, u32),
    kind: Kind,
    /// Column, row — in tiles.
    cursor: (u32, u32),
    undo: Vec<Edit>,
    redo: Vec<Edit>,
}

impl Tileset {
    pub fn new(size: (u32, u32), kind: Kind) -> Self {
        Self { size, kind, cursor: (0, 0), undo: Vec::new(), redo: Vec::new() }
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    pub fn set_kind(&mut self, kind: Kind) {
        self.kind = kind;
    }

    pub fn cursor(&self) -> (u32, u32) {
        self.cursor
    }

    /// Columns and rows of whole tiles a `width`×`height` sheet holds. The
    /// remainder past the last whole tile is not a tile.
    pub fn grid(&self, width: u32, height: u32) -> (u32, u32) {
        (width / self.size.0, height / self.size.1)
    }

    /// A new tile size; the cursor is re-clamped to the grid it makes.
    pub fn set_size(&mut self, size: (u32, u32), width: u32, height: u32) {
        self.size = size;
        let grid = self.grid(width, height);
        self.clamp(grid);
    }

    /// The cursor's tile in image pixels: x, y, width, height.
    pub fn cursor_rect(&self) -> (u32, u32, u32, u32) {
        (self.cursor.0 * self.size.0, self.cursor.1 * self.size.1, self.size.0, self.size.1)
    }

    pub fn move_by(&mut self, dx: i64, dy: i64, grid: (u32, u32)) {
        self.cursor.0 = (self.cursor.0 as i64 + dx).clamp(0, u32::MAX as i64) as u32;
        self.cursor.1 = (self.cursor.1 as i64 + dy).clamp(0, u32::MAX as i64) as u32;
        self.clamp(grid);
    }

    /// `0` and `$`: a column, or `None` for the last one.
    pub fn to_col(&mut self, col: Option<u32>, grid: (u32, u32)) {
        self.cursor.0 = col.unwrap_or(u32::MAX);
        self.clamp(grid);
    }

    /// `gg`, `G`, `5G`: a row, or `None` for the last one.
    pub fn to_row(&mut self, row: Option<u32>, grid: (u32, u32)) {
        self.cursor.1 = row.unwrap_or(u32::MAX);
        self.clamp(grid);
    }

    fn clamp(&mut self, grid: (u32, u32)) {
        self.cursor.0 = self.cursor.0.min(grid.0.saturating_sub(1));
        self.cursor.1 = self.cursor.1.min(grid.1.saturating_sub(1));
    }

    /// Whether the cursor's tile lies wholly inside a sheet `width` wide
    /// holding `rgba`. False for a sheet too small to hold one tile.
    fn in_sheet(&self, rgba: &[u8], width: u32) -> bool {
        let height = if width == 0 { 0 } else { rgba.len() as u32 / 4 / width };
        let grid = self.grid(width, height);
        self.cursor.0 < grid.0 && self.cursor.1 < grid.1
    }

    /// The tile under the cursor, copied out. `None` when the sheet has no
    /// whole tile there.
    pub fn yank(&self, rgba: &[u8], width: u32) -> Option<Tile> {
        if !self.in_sheet(rgba, width) {
            return None;
        }
        Some(Tile { width: self.size.0, height: self.size.1, rgba: self.read(rgba, width) })
    }

    /// `dd`: the tile copied out, transparent left behind.
    pub fn cut(&mut self, rgba: &mut [u8], width: u32) -> Option<Tile> {
        let tile = self.yank(rgba, width)?;
        let cleared = vec![0u8; tile.rgba.len()];
        self.apply(rgba, width, cleared);
        Some(tile)
    }

    /// `p`: `tile` written over the cursor's. A tile of another size is
    /// refused rather than clipped — a clipped paste is a guess about
    /// which corner you meant.
    pub fn paste(&mut self, rgba: &mut [u8], width: u32, tile: &Tile) -> Result<(), String> {
        if (tile.width, tile.height) != self.size {
            return Err(format!(
                "tile is {}×{}, grid is {}×{}",
                tile.width, tile.height, self.size.0, self.size.1
            ));
        }
        if !self.in_sheet(rgba, width) {
            return Err("no tile here".into());
        }
        self.apply(rgba, width, tile.rgba.clone());
        Ok(())
    }

    /// `r` and a direction: the cursor's tile turned in place, one undo
    /// step. Refused, and nothing recorded, for a quarter turn of a tile
    /// that is not square.
    pub fn turn(&mut self, rgba: &mut [u8], width: u32, turn: Turn) -> Result<(), String> {
        if !self.in_sheet(rgba, width) {
            return Err("no tile here".into());
        }
        let turned = turn.apply(&self.read(rgba, width), self.size.0, self.size.1)?;
        self.apply(rgba, width, turned);
        Ok(())
    }

    pub fn undo(&mut self, rgba: &mut [u8], width: u32) -> bool {
        let Some(edit) = self.undo.pop() else { return false };
        self.write(rgba, width, edit.at, &edit.before);
        self.redo.push(edit);
        true
    }

    pub fn redo(&mut self, rgba: &mut [u8], width: u32) -> bool {
        let Some(edit) = self.redo.pop() else { return false };
        self.write(rgba, width, edit.at, &edit.after);
        self.undo.push(edit);
        true
    }

    /// One recorded edit at the cursor: what was there goes on the undo
    /// stack, `after` goes on the sheet, redo is forgotten.
    fn apply(&mut self, rgba: &mut [u8], width: u32, after: Vec<u8>) {
        let before = self.read(rgba, width);
        self.write(rgba, width, self.cursor, &after);
        self.undo.push(Edit { at: self.cursor, before, after });
        self.redo.clear();
    }

    fn read(&self, rgba: &[u8], width: u32) -> Vec<u8> {
        let (x, y, w, h) = self.cursor_rect();
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for row in y..y + h {
            let start = ((row * width + x) * 4) as usize;
            out.extend_from_slice(&rgba[start..start + (w * 4) as usize]);
        }
        out
    }

    fn write(&self, rgba: &mut [u8], width: u32, at: (u32, u32), pixels: &[u8]) {
        let (w, h) = self.size;
        let (x, y) = (at.0 * w, at.1 * h);
        for (i, row) in (y..y + h).enumerate() {
            let start = ((row * width + x) * 4) as usize;
            let src = &pixels[i * (w * 4) as usize..(i + 1) * (w * 4) as usize];
            rgba[start..start + (w * 4) as usize].copy_from_slice(src);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `w`×`h` sheet whose every pixel is its own tile-independent value,
    /// so a copied tile can be told from any other.
    fn sheet(w: u32, h: u32) -> Vec<u8> {
        (0..w * h * 4).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn a_size_is_a_pair_or_a_square() {
        assert_eq!(parse_size("16x16"), Ok((16, 16)));
        assert_eq!(parse_size("16"), Ok((16, 16)));
        assert_eq!(parse_size("8X24"), Ok((8, 24)));
        assert!(parse_size("0").is_err());
        assert!(parse_size("16x").is_err());
        assert!(parse_size("big").is_err());
    }

    #[test]
    fn hex_parses_and_is_not_built() {
        assert_eq!(Kind::parse("tile"), Some(Kind::Tile));
        assert_eq!(Kind::parse("hex"), Some(Kind::Hex));
        assert_eq!(Kind::parse("square"), None);
        assert!(Kind::Hex.refusal().is_some());
        assert!(Kind::Tile.refusal().is_none());
    }

    #[test]
    fn the_grid_is_whole_tiles_only() {
        let map = Tileset::new((16, 16), Kind::Tile);
        assert_eq!(map.grid(100, 100), (6, 6));
        assert_eq!(map.grid(32, 15), (2, 0), "not tall enough for one row");
    }

    #[test]
    fn moves_clamp_to_the_grid_and_counts_multiply() {
        let mut map = Tileset::new((16, 16), Kind::Tile);
        let grid = map.grid(100, 100);
        map.move_by(2, 3, grid);
        assert_eq!(map.cursor(), (2, 3));
        map.move_by(-9, -9, grid);
        assert_eq!(map.cursor(), (0, 0));
        map.move_by(99, 99, grid);
        assert_eq!(map.cursor(), (5, 5));
    }

    #[test]
    fn the_edge_keys_go_to_the_grids_edges() {
        let mut map = Tileset::new((16, 16), Kind::Tile);
        let grid = map.grid(100, 100);
        map.to_col(None, grid);
        assert_eq!(map.cursor().0, 5, "$ is the last column");
        map.to_col(Some(0), grid);
        assert_eq!(map.cursor().0, 0);
        map.to_row(None, grid);
        assert_eq!(map.cursor().1, 5, "G is the last row");
        map.to_row(Some(4), grid);
        assert_eq!(map.cursor().1, 4, "5G is row 5, counted from one");
        map.to_row(Some(40), grid);
        assert_eq!(map.cursor().1, 5, "clamped");
    }

    #[test]
    fn a_size_change_reclamps_the_cursor() {
        let mut map = Tileset::new((16, 16), Kind::Tile);
        let grid = map.grid(100, 100);
        map.move_by(5, 5, grid);
        map.set_size((32, 32), 100, 100);
        assert_eq!(map.cursor(), (2, 2));
    }

    #[test]
    fn the_cursor_rect_is_in_pixels() {
        let mut map = Tileset::new((16, 8), Kind::Tile);
        map.move_by(2, 3, map.grid(100, 100));
        assert_eq!(map.cursor_rect(), (32, 24, 16, 8));
    }

    #[test]
    fn yank_reads_the_tile_under_the_cursor() {
        let mut map = Tileset::new((2, 2), Kind::Tile);
        let px = sheet(4, 4);
        map.move_by(1, 1, map.grid(4, 4));
        let tile = map.yank(&px, 4).unwrap();
        assert_eq!((tile.width, tile.height), (2, 2));
        // Rows 2 and 3, columns 2 and 3 of a 4-wide sheet.
        let mut want = Vec::new();
        for y in 2..4 {
            want.extend_from_slice(&px[(y * 4 + 2) * 4..(y * 4 + 4) * 4]);
        }
        assert_eq!(tile.rgba, want);
    }

    #[test]
    fn cut_yanks_then_leaves_transparent_behind() {
        let mut map = Tileset::new((2, 2), Kind::Tile);
        let mut px = sheet(4, 4);
        let before = px.clone();
        let tile = map.cut(&mut px, 4).unwrap();
        assert_eq!(
            tile.rgba,
            before[0..8].iter().chain(&before[16..24]).copied().collect::<Vec<_>>()
        );
        assert!(px[0..8].iter().all(|&b| b == 0));
        assert!(px[16..24].iter().all(|&b| b == 0));
        assert_eq!(&px[8..16], &before[8..16], "the neighbour is untouched");
    }

    #[test]
    fn paste_writes_the_tile_over_the_cursor() {
        let mut map = Tileset::new((2, 2), Kind::Tile);
        let mut px = sheet(4, 4);
        let tile = map.yank(&px, 4).unwrap();
        map.move_by(1, 1, map.grid(4, 4));
        map.paste(&mut px, 4, &tile).unwrap();
        assert_eq!(map.yank(&px, 4).unwrap().rgba, tile.rgba);
    }

    #[test]
    fn a_mismatched_tile_is_refused() {
        let mut map = Tileset::new((2, 2), Kind::Tile);
        let mut px = sheet(4, 4);
        let tile = Tile { width: 1, height: 1, rgba: vec![0; 4] };
        let err = map.paste(&mut px, 4, &tile).unwrap_err();
        assert_eq!(err, "tile is 1×1, grid is 2×2");
        assert_eq!(px, sheet(4, 4), "nothing changed");
    }

    #[test]
    fn undo_restores_what_cut_cleared_and_redo_clears_it_again() {
        let mut map = Tileset::new((2, 2), Kind::Tile);
        let mut px = sheet(4, 4);
        let before = px.clone();
        map.cut(&mut px, 4).unwrap();
        assert!(map.undo(&mut px, 4));
        assert_eq!(px, before);
        assert!(!map.undo(&mut px, 4), "nothing left");
        assert!(map.redo(&mut px, 4));
        assert!(px[0..8].iter().all(|&b| b == 0));
        assert!(!map.redo(&mut px, 4));
    }

    #[test]
    fn a_new_edit_drops_redo() {
        let mut map = Tileset::new((2, 2), Kind::Tile);
        let mut px = sheet(4, 4);
        map.cut(&mut px, 4).unwrap();
        map.undo(&mut px, 4);
        map.move_by(1, 0, map.grid(4, 4));
        map.cut(&mut px, 4).unwrap();
        assert!(!map.redo(&mut px, 4));
    }

    #[test]
    fn a_sheet_smaller_than_a_tile_has_no_tile_to_yank() {
        let mut map = Tileset::new((8, 8), Kind::Tile);
        let mut px = sheet(4, 4);
        assert!(map.yank(&px, 4).is_none());
        assert!(map.cut(&mut px, 4).is_none());
    }

    /// A 2×2 tile whose four pixels are all different, so every turn and
    /// mirror lands somewhere it can be told apart.
    fn abcd() -> Vec<u8> {
        [[1, 0, 0, 255], [2, 0, 0, 255], [3, 0, 0, 255], [4, 0, 0, 255]].concat()
    }

    fn corners(px: &[u8]) -> [u8; 4] {
        [px[0], px[4], px[8], px[12]]
    }

    #[test]
    fn turns_and_mirrors_land_where_they_say() {
        let mut map = Tileset::new((2, 2), Kind::Tile);
        let mut px = abcd();
        // a b      c a
        // c d  ->  d b   (a quarter turn right)
        map.turn(&mut px, 2, Turn::Right).unwrap();
        assert_eq!(corners(&px), [3, 1, 4, 2]);
        map.turn(&mut px, 2, Turn::Left).unwrap();
        assert_eq!(corners(&px), [1, 2, 3, 4], "and back");
        // a b      d c
        // c d  ->  b a   (half way round)
        map.turn(&mut px, 2, Turn::Half).unwrap();
        assert_eq!(corners(&px), [4, 3, 2, 1]);
        map.turn(&mut px, 2, Turn::Half).unwrap();
        // a b      b a
        // c d  ->  d c   (left-right mirror)
        map.turn(&mut px, 2, Turn::MirrorX).unwrap();
        assert_eq!(corners(&px), [2, 1, 4, 3]);
        map.turn(&mut px, 2, Turn::MirrorX).unwrap();
        // a b      c d
        // c d  ->  a b   (top-bottom mirror)
        map.turn(&mut px, 2, Turn::MirrorY).unwrap();
        assert_eq!(corners(&px), [3, 4, 1, 2]);
    }

    #[test]
    fn four_quarter_turns_are_the_tile_it_was() {
        let mut map = Tileset::new((2, 2), Kind::Tile);
        let mut px = abcd();
        for _ in 0..4 {
            map.turn(&mut px, 2, Turn::Right).unwrap();
        }
        assert_eq!(px, abcd());
    }

    #[test]
    fn a_turn_is_one_undo_step() {
        let mut map = Tileset::new((2, 2), Kind::Tile);
        let mut px = abcd();
        map.turn(&mut px, 2, Turn::Half).unwrap();
        map.turn(&mut px, 2, Turn::MirrorX).unwrap();
        assert!(map.undo(&mut px, 2));
        assert_eq!(corners(&px), [4, 3, 2, 1], "only the mirror came off");
        assert!(map.undo(&mut px, 2));
        assert_eq!(px, abcd());
    }

    #[test]
    fn a_quarter_turn_needs_a_square_tile() {
        let mut map = Tileset::new((2, 1), Kind::Tile);
        let mut px = [[1, 0, 0, 255], [2, 0, 0, 255]].concat();
        assert_eq!(map.turn(&mut px, 2, Turn::Right), Err("2×1 does not turn".to_string()));
        assert_eq!(map.turn(&mut px, 2, Turn::Left), Err("2×1 does not turn".to_string()));
        assert_eq!(px[0], 1, "unchanged");
        map.turn(&mut px, 2, Turn::Half).unwrap();
        assert_eq!((px[0], px[4]), (2, 1), "the half turn works at any size");
        assert!(map.undo(&mut px, 2), "the half turn is one step");
        assert!(!map.undo(&mut px, 2), "and the refusals recorded nothing");
    }
}
