//! A grid of tiles over an image, a cursor on it, and the block of tiles
//! the cursor selects.
//!
//! State and pixel arithmetic only, like [`crate::img::Img`] — the image
//! owns the pixels and lends them here as a [`Sheet`] to read, write and
//! resize. See `docs/specs/tileset.md`.

/// One block's pixels, RGBA8 — what the session's slot holds between a
/// `yy` on one sheet and a `p` on another. One tile, or the selection's
/// worth of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tile {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// The image's pixels and dimensions, lent to the tileset for one
/// operation. A resize changes all three, which is why the dimensions come
/// as `&mut` rather than by value.
pub struct Sheet<'a> {
    pub rgba: &'a mut Vec<u8>,
    pub width: &'a mut u32,
    pub height: &'a mut u32,
}

impl Sheet<'_> {
    fn dims(&self) -> (u32, u32) {
        (*self.width, *self.height)
    }
}

/// The shape of a tile. `Hex` parses so the spelling is reserved, and is
/// refused until someone builds it.
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

/// `16,16`, or `16` for both. A comma, never an `x`; zero is not a size.
pub fn parse_pair(s: &str) -> Result<(u32, u32), String> {
    let bad = || format!("not a pair: {s} (want 16 or 16,16)");
    let (w, h) = match s.split_once(',') {
        Some((w, h)) => (w, h),
        None => (s, s),
    };
    let w = w.trim().parse::<u32>().map_err(|_| bad())?;
    let h = h.trim().parse::<u32>().map_err(|_| bad())?;
    if w == 0 || h == 0 {
        return Err(bad());
    }
    Ok((w, h))
}

/// `1,1`, `-1,0`, or `2` for both — a change in tiles, either way.
pub fn parse_delta(s: &str) -> Result<(i64, i64), String> {
    let bad = || format!("not a change: {s} (want 1 or 1,1, negative to shrink)");
    let (w, h) = match s.split_once(',') {
        Some((w, h)) => (w, h),
        None => (s, s),
    };
    let w = w.trim().parse::<i64>().map_err(|_| bad())?;
    let h = h.trim().parse::<i64>().map_err(|_| bad())?;
    Ok((w, h))
}

/// `r` and a direction: how the selected block is turned in place.
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
    /// `pixels` of a `w`×`h` block, turned. A quarter turn of a block that
    /// is not square would not fit where it came from, and is refused.
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

/// One step of undo: a block overwritten, or the canvas resized.
#[derive(Debug, Clone)]
enum Edit {
    /// A rectangle of pixels — where, and both versions.
    Block { rect: (u32, u32, u32, u32), before: Vec<u8>, after: Vec<u8> },
    /// The whole sheet before a resize, and the size it became. Redo is
    /// the resize again, which is deterministic; undo puts the pixels back.
    Canvas { before: (u32, u32), pixels: Vec<u8>, after: (u32, u32) },
}

#[derive(Debug, Clone)]
pub struct Tileset {
    /// One tile, in pixels.
    size: (u32, u32),
    kind: Kind,
    /// Column, row — in tiles. The top-left of the selection.
    cursor: (u32, u32),
    /// Columns and rows the cursor selects. `(1, 1)` is one tile.
    select: (u32, u32),
    undo: Vec<Edit>,
    redo: Vec<Edit>,
}

impl Tileset {
    pub fn new(size: (u32, u32), kind: Kind) -> Self {
        Self { size, kind, cursor: (0, 0), select: (1, 1), undo: Vec::new(), redo: Vec::new() }
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

    pub fn select(&self) -> (u32, u32) {
        self.select
    }

    /// Columns and rows of whole tiles a `width`×`height` sheet holds. The
    /// remainder past the last whole tile is not a tile.
    pub fn grid(&self, width: u32, height: u32) -> (u32, u32) {
        (width / self.size.0, height / self.size.1)
    }

    /// A new tile size; selection and cursor are re-clamped to the grid it
    /// makes.
    pub fn set_size(&mut self, size: (u32, u32), width: u32, height: u32) {
        self.size = size;
        self.fit(width, height);
    }

    /// How many tiles the cursor selects. Clamped to the grid, never less
    /// than one.
    pub fn set_select(&mut self, select: (u32, u32), width: u32, height: u32) {
        self.select = select;
        self.fit(width, height);
    }

    /// The selection in image pixels: x, y, width, height.
    pub fn cursor_rect(&self) -> (u32, u32, u32, u32) {
        (
            self.cursor.0 * self.size.0,
            self.cursor.1 * self.size.1,
            self.select.0 * self.size.0,
            self.select.1 * self.size.1,
        )
    }

    /// `hjkl`: by one tile, whatever the selection.
    pub fn move_by(&mut self, dx: i64, dy: i64, grid: (u32, u32)) {
        self.cursor.0 = (self.cursor.0 as i64 + dx).clamp(0, u32::MAX as i64) as u32;
        self.cursor.1 = (self.cursor.1 as i64 + dy).clamp(0, u32::MAX as i64) as u32;
        self.clamp(grid);
    }

    /// `0` and `$`: a column, or `None` for the last one the selection
    /// can start in.
    pub fn to_col(&mut self, col: Option<u32>, grid: (u32, u32)) {
        self.cursor.0 = col.unwrap_or(u32::MAX);
        self.clamp(grid);
    }

    /// `gg`, `G`, `5G`: a row, or `None` for the last.
    pub fn to_row(&mut self, row: Option<u32>, grid: (u32, u32)) {
        self.cursor.1 = row.unwrap_or(u32::MAX);
        self.clamp(grid);
    }

    /// Selection inside the grid, cursor inside what is left.
    fn fit(&mut self, width: u32, height: u32) {
        let grid = self.grid(width, height);
        self.select.0 = self.select.0.clamp(1, grid.0.max(1));
        self.select.1 = self.select.1.clamp(1, grid.1.max(1));
        self.clamp(grid);
    }

    /// The cursor cannot carry the selection past the grid's edge.
    fn clamp(&mut self, grid: (u32, u32)) {
        self.cursor.0 = self.cursor.0.min(grid.0.saturating_sub(self.select.0));
        self.cursor.1 = self.cursor.1.min(grid.1.saturating_sub(self.select.1));
    }

    /// Whether the selection lies wholly inside a `width`×`height` sheet.
    /// False for one too small to hold it.
    fn in_sheet(&self, width: u32, height: u32) -> bool {
        let (x, y, w, h) = self.cursor_rect();
        x + w <= width && y + h <= height
    }

    /// The selection, copied out of a `width`×`height` sheet. `None` when
    /// the sheet does not hold it. Plain pixels rather than a [`Sheet`]:
    /// a yank changes nothing, and a `&self` caller has no `&mut` to lend.
    pub fn yank(&self, rgba: &[u8], width: u32, height: u32) -> Option<Tile> {
        if !self.in_sheet(width, height) {
            return None;
        }
        let rect = self.cursor_rect();
        Some(Tile { width: rect.2, height: rect.3, rgba: read_from(rgba, width, rect) })
    }

    /// `dd`: the selection copied out, transparent left behind.
    pub fn cut(&mut self, sheet: &mut Sheet) -> Option<Tile> {
        let tile = self.yank(sheet.rgba, *sheet.width, *sheet.height)?;
        let cleared = vec![0u8; tile.rgba.len()];
        self.block(sheet, self.cursor_rect(), cleared);
        Some(tile)
    }

    /// `p`: `tile` written with its top-left at the cursor's, whatever
    /// size it is — a block yanked wide pastes wide. Refused, not clipped,
    /// when it runs past the sheet.
    pub fn paste(&mut self, sheet: &mut Sheet, tile: &Tile) -> Result<(), String> {
        let (x, y, ..) = self.cursor_rect();
        let (width, height) = sheet.dims();
        if x + tile.width > width || y + tile.height > height {
            return Err(format!(
                "{}×{} does not fit at {},{}",
                tile.width, tile.height, self.cursor.0, self.cursor.1
            ));
        }
        self.block(sheet, (x, y, tile.width, tile.height), tile.rgba.clone());
        Ok(())
    }

    /// `r` and a direction: the selection turned in place, one undo step.
    /// Refused, and nothing recorded, for a quarter turn of a block that
    /// is not square.
    pub fn turn(&mut self, sheet: &mut Sheet, turn: Turn) -> Result<(), String> {
        if !self.in_sheet(*sheet.width, *sheet.height) {
            return Err("no tile here".into());
        }
        let rect = self.cursor_rect();
        let turned = turn.apply(&read(sheet, rect), rect.2, rect.3)?;
        self.block(sheet, rect, turned);
        Ok(())
    }

    /// `image resize 32,32`: the canvas becomes that many tiles a side,
    /// old pixels top-left, new room transparent, the rest cropped away.
    pub fn resize(&mut self, sheet: &mut Sheet, tiles: (u32, u32)) -> Result<(), String> {
        if tiles.0 == 0 || tiles.1 == 0 {
            return Err(format!("a canvas of {},{} tiles is nothing", tiles.0, tiles.1));
        }
        let after = (tiles.0 * self.size.0, tiles.1 * self.size.1);
        let before = sheet.dims();
        let pixels = std::mem::take(sheet.rgba);
        *sheet.rgba = resized(&pixels, before, after);
        (*sheet.width, *sheet.height) = after;
        self.undo.push(Edit::Canvas { before, pixels, after });
        self.redo.clear();
        self.fit(after.0, after.1);
        Ok(())
    }

    /// `image grow 1,1`: that many tiles added to the canvas it has —
    /// counted from its pixel size, so a sheet with a ragged edge keeps
    /// it. Negative shrinks, never below one tile.
    pub fn grow(&mut self, sheet: &mut Sheet, delta: (i64, i64)) -> Result<(), String> {
        let (width, height) = sheet.dims();
        let after = (
            (width as i64 + delta.0 * self.size.0 as i64).max(self.size.0 as i64) as u32,
            (height as i64 + delta.1 * self.size.1 as i64).max(self.size.1 as i64) as u32,
        );
        let before = sheet.dims();
        if after == before {
            return Ok(());
        }
        let pixels = std::mem::take(sheet.rgba);
        *sheet.rgba = resized(&pixels, before, after);
        (*sheet.width, *sheet.height) = after;
        self.undo.push(Edit::Canvas { before, pixels, after });
        self.redo.clear();
        self.fit(after.0, after.1);
        Ok(())
    }

    pub fn undo(&mut self, sheet: &mut Sheet) -> bool {
        let Some(edit) = self.undo.pop() else { return false };
        match &edit {
            Edit::Block { rect, before, .. } => write(sheet, *rect, before),
            Edit::Canvas { before, pixels, .. } => {
                *sheet.rgba = pixels.clone();
                (*sheet.width, *sheet.height) = *before;
                self.fit(before.0, before.1);
            }
        }
        self.redo.push(edit);
        true
    }

    pub fn redo(&mut self, sheet: &mut Sheet) -> bool {
        let Some(edit) = self.redo.pop() else { return false };
        match &edit {
            Edit::Block { rect, after, .. } => write(sheet, *rect, after),
            Edit::Canvas { before, pixels, after } => {
                *sheet.rgba = resized(pixels, *before, *after);
                (*sheet.width, *sheet.height) = *after;
                self.fit(after.0, after.1);
            }
        }
        self.undo.push(edit);
        true
    }

    /// One recorded block edit: what was there goes on the undo stack,
    /// `after` goes on the sheet, redo is forgotten.
    fn block(&mut self, sheet: &mut Sheet, rect: (u32, u32, u32, u32), after: Vec<u8>) {
        let before = read(sheet, rect);
        write(sheet, rect, &after);
        self.undo.push(Edit::Block { rect, before, after });
        self.redo.clear();
    }
}

/// `pixels` of a `before` sheet laid top-left onto an `after` one —
/// cropped where `after` is smaller, transparent where it is larger.
fn resized(pixels: &[u8], before: (u32, u32), after: (u32, u32)) -> Vec<u8> {
    let mut out = vec![0u8; (after.0 * after.1 * 4) as usize];
    let (w, h) = (before.0.min(after.0), before.1.min(after.1));
    for y in 0..h {
        let src = ((y * before.0) * 4) as usize;
        let dst = ((y * after.0) * 4) as usize;
        out[dst..dst + (w * 4) as usize].copy_from_slice(&pixels[src..src + (w * 4) as usize]);
    }
    out
}

fn read(sheet: &Sheet, rect: (u32, u32, u32, u32)) -> Vec<u8> {
    read_from(sheet.rgba, *sheet.width, rect)
}

fn read_from(rgba: &[u8], width: u32, (x, y, w, h): (u32, u32, u32, u32)) -> Vec<u8> {
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for row in y..y + h {
        let start = ((row * width + x) * 4) as usize;
        out.extend_from_slice(&rgba[start..start + (w * 4) as usize]);
    }
    out
}

fn write(sheet: &mut Sheet, (x, y, w, h): (u32, u32, u32, u32), pixels: &[u8]) {
    let width = *sheet.width;
    for (i, row) in (y..y + h).enumerate() {
        let start = ((row * width + x) * 4) as usize;
        let src = &pixels[i * (w * 4) as usize..(i + 1) * (w * 4) as usize];
        sheet.rgba[start..start + (w * 4) as usize].copy_from_slice(src);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `w`×`h` sheet whose every byte is its own value mod 251, so a
    /// copied block can be told from any other.
    fn pixels(w: u32, h: u32) -> Vec<u8> {
        (0..w * h * 4).map(|i| (i % 251) as u8).collect()
    }

    /// The sheet a test edits: pixels and dimensions the tileset may change.
    struct Canvas {
        rgba: Vec<u8>,
        w: u32,
        h: u32,
    }

    impl Canvas {
        fn new(w: u32, h: u32) -> Self {
            Self { rgba: pixels(w, h), w, h }
        }

        fn sheet(&mut self) -> Sheet<'_> {
            Sheet { rgba: &mut self.rgba, width: &mut self.w, height: &mut self.h }
        }

        fn at(&self, x: u32, y: u32) -> &[u8] {
            let i = ((y * self.w + x) * 4) as usize;
            &self.rgba[i..i + 4]
        }
    }

    #[test]
    fn a_pair_is_two_numbers_with_a_comma_or_one_for_both() {
        assert_eq!(parse_pair("16,16"), Ok((16, 16)));
        assert_eq!(parse_pair("16"), Ok((16, 16)));
        assert_eq!(parse_pair("8, 24"), Ok((8, 24)));
        assert_eq!(parse_pair("16x16"), Err("not a pair: 16x16 (want 16 or 16,16)".into()));
        assert!(parse_pair("0").is_err());
        assert!(parse_pair("16,").is_err());
        assert!(parse_pair("big").is_err());
    }

    #[test]
    fn a_delta_may_be_negative() {
        assert_eq!(parse_delta("1,1"), Ok((1, 1)));
        assert_eq!(parse_delta("-1,0"), Ok((-1, 0)));
        assert_eq!(parse_delta("2"), Ok((2, 2)));
        assert!(parse_delta("1x1").is_err());
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
    fn the_cursor_rect_is_the_selection_in_pixels() {
        let mut map = Tileset::new((16, 8), Kind::Tile);
        map.move_by(2, 3, map.grid(100, 100));
        assert_eq!(map.cursor_rect(), (32, 24, 16, 8));
        map.set_select((3, 2), 100, 100);
        assert_eq!(map.select(), (3, 2));
        assert_eq!(map.cursor_rect(), (32, 24, 48, 16));
    }

    /// The selection stays inside the grid: it is clamped to the grid when
    /// set, and the cursor cannot carry it past the edge.
    #[test]
    fn the_selection_keeps_inside_the_grid() {
        let mut map = Tileset::new((16, 16), Kind::Tile);
        map.set_select((10, 2), 100, 100);
        assert_eq!(map.select(), (6, 2), "no wider than the grid");
        map.set_select((0, 0), 100, 100);
        assert_eq!(map.select(), (1, 1), "never nothing");

        map.set_select((3, 3), 100, 100);
        let grid = map.grid(100, 100);
        map.move_by(99, 99, grid);
        assert_eq!(map.cursor(), (3, 3), "6 columns, 3 selected: column 3 is the last start");
        map.to_col(None, grid);
        assert_eq!(map.cursor().0, 3);

        map.set_size((32, 32), 100, 100);
        assert_eq!(map.select(), (3, 3), "a 3×3 grid holds it");
        assert_eq!(map.cursor(), (0, 0));
    }

    #[test]
    fn yank_reads_the_block_under_the_cursor() {
        let mut map = Tileset::new((2, 2), Kind::Tile);
        let c = Canvas::new(6, 6);
        map.move_by(1, 1, map.grid(6, 6));
        map.set_select((2, 1), 6, 6);
        let tile = map.yank(&c.rgba, c.w, c.h).unwrap();
        assert_eq!((tile.width, tile.height), (4, 2));
        let mut want = Vec::new();
        for y in 2..4 {
            want.extend_from_slice(&c.rgba[(y * 6 + 2) * 4..(y * 6 + 6) * 4]);
        }
        assert_eq!(tile.rgba, want);
    }

    #[test]
    fn cut_yanks_then_leaves_transparent_behind() {
        let mut map = Tileset::new((2, 2), Kind::Tile);
        let mut c = Canvas::new(4, 4);
        let before = c.rgba.clone();
        let tile = map.cut(&mut c.sheet()).unwrap();
        assert_eq!(tile.rgba, [&before[0..8], &before[16..24]].concat());
        assert!(c.rgba[0..8].iter().all(|&b| b == 0));
        assert!(c.rgba[16..24].iter().all(|&b| b == 0));
        assert_eq!(&c.rgba[8..16], &before[8..16], "the neighbour is untouched");
    }

    #[test]
    fn paste_writes_the_tile_over_the_cursor() {
        let mut map = Tileset::new((2, 2), Kind::Tile);
        let mut c = Canvas::new(4, 4);
        let tile = map.yank(&c.rgba, c.w, c.h).unwrap();
        map.move_by(1, 1, map.grid(4, 4));
        map.paste(&mut c.sheet(), &tile).unwrap();
        assert_eq!(map.yank(&c.rgba, c.w, c.h).unwrap().rgba, tile.rgba);
    }

    /// A block yanked with a wide selection pastes as that block wherever
    /// the cursor is, selection or no — what fits, fits.
    #[test]
    fn a_block_pastes_at_the_cursor_whatever_the_selection_is() {
        let mut map = Tileset::new((2, 2), Kind::Tile);
        let mut c = Canvas::new(8, 4);
        map.set_select((2, 2), 8, 4);
        let block = map.yank(&c.rgba, c.w, c.h).unwrap();
        assert_eq!((block.width, block.height), (4, 4));

        map.set_select((1, 1), 8, 4);
        map.move_by(2, 0, map.grid(8, 4));
        map.paste(&mut c.sheet(), &block).unwrap();
        map.set_select((2, 2), 8, 4);
        assert_eq!(map.yank(&c.rgba, c.w, c.h).unwrap().rgba, block.rgba);
    }

    #[test]
    fn a_paste_that_runs_past_the_sheet_is_refused() {
        let mut map = Tileset::new((2, 2), Kind::Tile);
        let mut c = Canvas::new(4, 4);
        let before = c.rgba.clone();
        let tile = Tile { width: 6, height: 2, rgba: vec![0; 48] };
        assert_eq!(map.paste(&mut c.sheet(), &tile), Err("6×2 does not fit at 0,0".into()));
        map.move_by(1, 1, map.grid(4, 4));
        let tile = Tile { width: 3, height: 3, rgba: vec![0; 36] };
        assert_eq!(map.paste(&mut c.sheet(), &tile), Err("3×3 does not fit at 1,1".into()));
        assert_eq!(c.rgba, before, "nothing changed");
    }

    #[test]
    fn undo_restores_what_cut_cleared_and_redo_clears_it_again() {
        let mut map = Tileset::new((2, 2), Kind::Tile);
        let mut c = Canvas::new(4, 4);
        let before = c.rgba.clone();
        map.cut(&mut c.sheet()).unwrap();
        assert!(map.undo(&mut c.sheet()));
        assert_eq!(c.rgba, before);
        assert!(!map.undo(&mut c.sheet()), "nothing left");
        assert!(map.redo(&mut c.sheet()));
        assert!(c.rgba[0..8].iter().all(|&b| b == 0));
        assert!(!map.redo(&mut c.sheet()));
    }

    #[test]
    fn a_new_edit_drops_redo() {
        let mut map = Tileset::new((2, 2), Kind::Tile);
        let mut c = Canvas::new(4, 4);
        map.cut(&mut c.sheet()).unwrap();
        map.undo(&mut c.sheet());
        map.move_by(1, 0, map.grid(4, 4));
        map.cut(&mut c.sheet()).unwrap();
        assert!(!map.redo(&mut c.sheet()));
    }

    #[test]
    fn a_sheet_smaller_than_a_tile_has_no_tile_to_yank() {
        let mut map = Tileset::new((8, 8), Kind::Tile);
        let mut c = Canvas::new(4, 4);
        assert!(map.yank(&c.rgba, c.w, c.h).is_none());
        assert!(map.cut(&mut c.sheet()).is_none());
    }

    /// `resize 3,3` in tiles: the canvas becomes three tiles a side, the old
    /// pixels stay top-left, the new room is transparent; smaller crops.
    #[test]
    fn resize_is_in_tiles_padding_transparent_and_cropping() {
        let mut map = Tileset::new((2, 2), Kind::Tile);
        let mut c = Canvas::new(4, 4);
        let before = c.rgba.clone();
        map.resize(&mut c.sheet(), (3, 3)).unwrap();
        assert_eq!((c.w, c.h), (6, 6));
        assert_eq!(c.at(3, 3), &before[(3 * 4 + 3) * 4..(3 * 4 + 4) * 4], "old pixels stay put");
        assert_eq!(c.at(5, 5), &[0, 0, 0, 0], "new room is clear");
        assert_eq!(c.at(5, 0), &[0, 0, 0, 0]);

        map.resize(&mut c.sheet(), (1, 1)).unwrap();
        assert_eq!((c.w, c.h), (2, 2));
        assert_eq!(c.at(1, 1), &before[(1 * 4 + 1) * 4..(1 * 4 + 2) * 4], "cropped, not scaled");

        assert_eq!(
            map.resize(&mut c.sheet(), (0, 3)),
            Err("a canvas of 0,3 tiles is nothing".into())
        );
    }

    #[test]
    fn a_resize_is_one_undo_step_that_brings_the_pixels_back() {
        let mut map = Tileset::new((2, 2), Kind::Tile);
        let mut c = Canvas::new(4, 4);
        let before = c.rgba.clone();
        map.resize(&mut c.sheet(), (1, 1)).unwrap();
        assert!(map.undo(&mut c.sheet()));
        assert_eq!((c.w, c.h), (4, 4));
        assert_eq!(c.rgba, before, "the cropped tiles came back");
        assert!(map.redo(&mut c.sheet()));
        assert_eq!((c.w, c.h), (2, 2));
        assert!(map.undo(&mut c.sheet()));
        assert_eq!(c.rgba, before);
    }

    #[test]
    fn grow_adds_tiles_to_the_current_canvas_and_shrinks_no_further_than_one() {
        let mut map = Tileset::new((2, 2), Kind::Tile);
        let mut c = Canvas::new(5, 4);
        map.grow(&mut c.sheet(), (1, 0)).unwrap();
        assert_eq!((c.w, c.h), (7, 4), "one tile wider, from the pixel width it had");
        map.grow(&mut c.sheet(), (-9, -9)).unwrap();
        assert_eq!((c.w, c.h), (2, 2), "never smaller than a tile");
    }

    #[test]
    fn the_cursor_and_selection_follow_a_smaller_canvas() {
        let mut map = Tileset::new((2, 2), Kind::Tile);
        let mut c = Canvas::new(8, 8);
        map.set_select((2, 2), 8, 8);
        map.move_by(2, 2, map.grid(8, 8));
        map.resize(&mut c.sheet(), (2, 2)).unwrap();
        assert_eq!(map.cursor(), (0, 0));
        assert_eq!(map.select(), (2, 2));
        map.resize(&mut c.sheet(), (1, 1)).unwrap();
        assert_eq!(map.select(), (1, 1));
    }

    /// A 2×2 tile whose four pixels are all different, so every turn and
    /// mirror lands somewhere it can be told apart.
    fn abcd() -> Canvas {
        let rgba = [[1, 0, 0, 255], [2, 0, 0, 255], [3, 0, 0, 255], [4, 0, 0, 255]].concat();
        Canvas { rgba, w: 2, h: 2 }
    }

    fn corners(c: &Canvas) -> [u8; 4] {
        [c.rgba[0], c.rgba[4], c.rgba[8], c.rgba[12]]
    }

    #[test]
    fn turns_and_mirrors_land_where_they_say() {
        let mut map = Tileset::new((2, 2), Kind::Tile);
        let mut c = abcd();
        // a b      c a
        // c d  ->  d b   (a quarter turn right)
        map.turn(&mut c.sheet(), Turn::Right).unwrap();
        assert_eq!(corners(&c), [3, 1, 4, 2]);
        map.turn(&mut c.sheet(), Turn::Left).unwrap();
        assert_eq!(corners(&c), [1, 2, 3, 4], "and back");
        // a b      d c
        // c d  ->  b a   (half way round)
        map.turn(&mut c.sheet(), Turn::Half).unwrap();
        assert_eq!(corners(&c), [4, 3, 2, 1]);
        map.turn(&mut c.sheet(), Turn::Half).unwrap();
        // a b      b a
        // c d  ->  d c   (left-right mirror)
        map.turn(&mut c.sheet(), Turn::MirrorX).unwrap();
        assert_eq!(corners(&c), [2, 1, 4, 3]);
        map.turn(&mut c.sheet(), Turn::MirrorX).unwrap();
        // a b      c d
        // c d  ->  a b   (top-bottom mirror)
        map.turn(&mut c.sheet(), Turn::MirrorY).unwrap();
        assert_eq!(corners(&c), [3, 4, 1, 2]);
    }

    #[test]
    fn four_quarter_turns_are_the_tile_it_was() {
        let mut map = Tileset::new((2, 2), Kind::Tile);
        let mut c = abcd();
        let before = c.rgba.clone();
        for _ in 0..4 {
            map.turn(&mut c.sheet(), Turn::Right).unwrap();
        }
        assert_eq!(c.rgba, before);
    }

    #[test]
    fn a_turn_is_one_undo_step() {
        let mut map = Tileset::new((2, 2), Kind::Tile);
        let mut c = abcd();
        let before = c.rgba.clone();
        map.turn(&mut c.sheet(), Turn::Half).unwrap();
        map.turn(&mut c.sheet(), Turn::MirrorX).unwrap();
        assert!(map.undo(&mut c.sheet()));
        assert_eq!(corners(&c), [4, 3, 2, 1], "only the mirror came off");
        assert!(map.undo(&mut c.sheet()));
        assert_eq!(c.rgba, before);
    }

    /// The whole selection turns as one block, so a 2×1 selection of 1×2
    /// tiles is square and turns; a 2×1 selection of square tiles is not.
    #[test]
    fn a_quarter_turn_needs_a_square_selection() {
        let mut map = Tileset::new((2, 1), Kind::Tile);
        let mut c = Canvas { rgba: [[1, 0, 0, 255], [2, 0, 0, 255]].concat(), w: 2, h: 1 };
        assert_eq!(map.turn(&mut c.sheet(), Turn::Right), Err("2×1 does not turn".into()));
        assert_eq!(c.rgba[0], 1, "unchanged");
        map.turn(&mut c.sheet(), Turn::Half).unwrap();
        assert_eq!((c.rgba[0], c.rgba[4]), (2, 1), "the half turn works at any size");
        assert!(map.undo(&mut c.sheet()), "the half turn is one step");
        assert!(!map.undo(&mut c.sheet()), "and the refusals recorded nothing");

        let mut map = Tileset::new((1, 2), Kind::Tile);
        let mut c = abcd();
        map.set_select((2, 1), 2, 2);
        map.turn(&mut c.sheet(), Turn::Right).unwrap();
        assert_eq!(corners(&c), [3, 1, 4, 2], "two tall tiles side by side are a square");
    }
}
