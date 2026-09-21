//! An opened image: pixels, dimensions, and a crop position.
//!
//! State only, like [`crate::tree::Tree`] — no terminal, no escape sequences.
//! The core decodes because pixel dimensions are core facts (the status line
//! and the scroll bounds are made of them) and because every frontend wants
//! the same bytes: a GUI blits `rgba`, the terminal frontend encodes it for
//! the wire. See `docs/specs/images.md`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::tileset::{Kind, Tile, Tileset};

#[derive(Debug, Clone)]
pub struct Img {
    pub path: PathBuf,
    /// Pixel dimensions, from the decode.
    pub width: u32,
    pub height: u32,
    /// RGBA8, row-major.
    pub rgba: Vec<u8>,
    /// Top-left of the visible region, in pixels, both clamped.
    scroll: (u32, u32),
    /// What the frontend last gave this pane, in pixels — the same
    /// arrangement as `Window::height`: the frontend reports the room, the
    /// core scrolls within it. Zero until the first frame, which clamps
    /// nothing away because the first frame arrives before the first key.
    viewport: (u32, u32),
    /// One `hjkl` step in pixels, set by the frontend beside the viewport —
    /// one text row's worth, so "a little" means the same distance it means
    /// in a file. Square, because pixels are.
    step: u32,
    /// Stable per opened image, for a frontend that uploads pixels to the
    /// terminal once and refers to them by number after.
    pub id: u64,
    /// The grid, while `:set editor tileset` is on. See
    /// `docs/specs/tileset.md`.
    tileset: Option<Tileset>,
    /// The tile size and kind outlive the grid: leaving and coming back
    /// finds them where they were.
    tile_size: (u32, u32),
    tile_kind: Kind,
    /// Edited since it was read or written. What `:q` reads.
    pub dirty: bool,
    /// Bumped by every edit. What a frontend that uploaded the pixels once
    /// reads, to know the upload is stale.
    pub generation: u64,
    /// Display pixels per image pixel. The core holds everything else in
    /// image pixels; the frontend divides the room it reports by this. See
    /// `docs/specs/zoom.md`.
    zoom: f32,
}

/// Below this a sheet is a dot; above it a pixel is a pane.
pub const ZOOM_RANGE: (f32, f32) = (0.05, 32.0);

/// Whether `path` is worth trying to decode at all.
///
/// Extension, not content sniffing: a misnamed file fails the decode and
/// falls back to text, which is the same answer sniffing would have bought
/// at higher cost.
pub fn looks_like_image(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else { return false };
    matches!(ext.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp")
}

impl Img {
    /// Decodes `path` — RGBA8, first frame of an animation.
    ///
    /// Failing here is what makes `:e` on a corrupt file honest: the caller
    /// falls back to opening the bytes as the text they always were.
    pub fn open(path: &Path, id: u64) -> Result<Self> {
        let decoded = image::ImageReader::open(path)
            .with_context(|| format!("reading {}", path.display()))?
            .with_guessed_format()
            .with_context(|| format!("reading {}", path.display()))?
            .decode()
            .with_context(|| format!("decoding {}", path.display()))?;
        let rgba = decoded.to_rgba8();
        let (width, height) = rgba.dimensions();
        Ok(Self::from_pixels(path.to_path_buf(), width, height, rgba.into_raw(), id))
    }

    /// The constructor the decode feeds, and the one a test can feed pixels
    /// to without a file.
    pub fn from_pixels(path: PathBuf, width: u32, height: u32, rgba: Vec<u8>, id: u64) -> Self {
        Self {
            path,
            width,
            height,
            rgba,
            scroll: (0, 0),
            viewport: (0, 0),
            step: 1,
            id,
            tileset: None,
            tile_size: (16, 16),
            tile_kind: Kind::Tile,
            dirty: false,
            generation: 0,
            zoom: 1.0,
        }
    }

    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    /// `:zoom 5`, `:zoom 0.1`, clamped to [`ZOOM_RANGE`].
    pub fn set_zoom(&mut self, zoom: f32) {
        self.zoom = zoom.clamp(ZOOM_RANGE.0, ZOOM_RANGE.1);
    }

    /// `:zoom +` doubles, `:zoom -` halves.
    pub fn zoom_step(&mut self, closer: bool) {
        self.set_zoom(if closer { self.zoom * 2.0 } else { self.zoom / 2.0 });
    }

    /// Writes the pixels as PNG — to `path` when given, re-pointing the
    /// image at it the way `:w other.rs` re-points a buffer, else to its
    /// own. Any other extension is refused rather than writing PNG bytes
    /// under a `.jpg` name.
    pub fn save(&mut self, path: Option<&Path>) -> std::result::Result<(), String> {
        let target = path.unwrap_or(&self.path).to_path_buf();
        let png = target
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("png"));
        if !png {
            return Err("only png".into());
        }
        image::save_buffer(
            &target,
            &self.rgba,
            self.width,
            self.height,
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| format!("error: {e}"))?;
        self.path = target;
        self.dirty = false;
        Ok(())
    }

    // ---- the tileset: see `docs/specs/tileset.md` ----

    pub fn tileset(&self) -> Option<&Tileset> {
        self.tileset.as_ref()
    }

    pub fn tile_size(&self) -> (u32, u32) {
        self.tile_size
    }

    pub fn tile_kind(&self) -> Kind {
        self.tile_kind
    }

    /// A new tile size, kept whether or not the grid is on; the cursor is
    /// re-clamped when it is.
    pub fn set_tile_size(&mut self, size: (u32, u32)) {
        self.tile_size = size;
        if let Some(map) = &mut self.tileset {
            map.set_size(size, self.width, self.height);
        }
        self.follow_cursor();
    }

    pub fn set_tile_kind(&mut self, kind: Kind) {
        self.tile_kind = kind;
        if let Some(map) = &mut self.tileset {
            map.set_kind(kind);
        }
    }

    /// `:set editor tileset`. A sheet too small for one tile has no grid
    /// to put a cursor on, and says so.
    pub fn enter_tileset(&mut self) -> std::result::Result<(), String> {
        if self.tileset.is_some() {
            return Ok(());
        }
        let map = Tileset::new(self.tile_size, self.tile_kind);
        let (cols, rows) = map.grid(self.width, self.height);
        if cols == 0 || rows == 0 {
            let (w, h) = self.tile_size;
            return Err(format!("{}×{} holds no {w}×{h} tile", self.width, self.height));
        }
        self.tileset = Some(map);
        self.follow_cursor();
        Ok(())
    }

    /// `Esc`, or `:set editor image`. Size, kind and edits stay.
    pub fn leave_tileset(&mut self) {
        self.tileset = None;
    }

    fn grid(&self) -> (u32, u32) {
        self.tileset.as_ref().map(|m| m.grid(self.width, self.height)).unwrap_or((0, 0))
    }

    /// `hjkl` on the grid, counts multiplied in.
    pub fn tile_move(&mut self, dx: i64, dy: i64) {
        let grid = self.grid();
        if let Some(map) = &mut self.tileset {
            map.move_by(dx, dy, grid);
        }
        self.follow_cursor();
    }

    /// `0` and `$`: a column, or the last.
    pub fn tile_to_col(&mut self, col: Option<u32>) {
        let grid = self.grid();
        if let Some(map) = &mut self.tileset {
            map.to_col(col, grid);
        }
        self.follow_cursor();
    }

    /// `gg`, `G`, `5G`: a row, or the last.
    pub fn tile_to_row(&mut self, row: Option<u32>) {
        let grid = self.grid();
        if let Some(map) = &mut self.tileset {
            map.to_row(row, grid);
        }
        self.follow_cursor();
    }

    /// `Ctrl-D` / `Ctrl-U`: half a viewport of rows, at least one.
    pub fn tile_half_page(&mut self, down: bool, count: usize) {
        let rows =
            (self.viewport.1 / 2 / self.tile_size.1.max(1)).max(1) as i64 * count.max(1) as i64;
        self.tile_move(0, if down { rows } else { -rows });
    }

    /// The least scroll that puts the cursor's tile wholly in the viewport;
    /// none when it already is. A viewport smaller than a tile shows the
    /// tile's top-left.
    fn follow_cursor(&mut self) {
        let Some(map) = &self.tileset else { return };
        let (x, y, w, h) = map.cursor_rect();
        let (vw, vh) = self.viewport;
        if vw == 0 || vh == 0 {
            return;
        }
        let (sx, sy) = self.scroll;
        let sx = if x < sx {
            x
        } else if x + w > sx + vw {
            (x + w).saturating_sub(vw).min(x)
        } else {
            sx
        };
        let sy = if y < sy {
            y
        } else if y + h > sy + vh {
            (y + h).saturating_sub(vh).min(y)
        } else {
            sy
        };
        self.scroll = (sx, sy);
        self.clamp();
    }

    /// `yy`.
    pub fn tile_yank(&self) -> Option<Tile> {
        self.tileset.as_ref()?.yank(&self.rgba, self.width)
    }

    /// `dd`: the tile, and transparent left behind.
    pub fn tile_cut(&mut self) -> Option<Tile> {
        let map = self.tileset.as_mut()?;
        let tile = map.cut(&mut self.rgba, self.width)?;
        self.edited();
        Some(tile)
    }

    /// `p`.
    pub fn tile_paste(&mut self, tile: &Tile) -> std::result::Result<(), String> {
        let Some(map) = self.tileset.as_mut() else { return Err("no grid here".into()) };
        map.paste(&mut self.rgba, self.width, tile)?;
        self.edited();
        Ok(())
    }

    /// `u`. False when there is nothing left to undo.
    pub fn tile_undo(&mut self) -> bool {
        let Some(map) = self.tileset.as_mut() else { return false };
        let done = map.undo(&mut self.rgba, self.width);
        if done {
            self.edited();
        }
        done
    }

    /// `Ctrl-R`.
    pub fn tile_redo(&mut self) -> bool {
        let Some(map) = self.tileset.as_mut() else { return false };
        let done = map.redo(&mut self.rgba, self.width);
        if done {
            self.edited();
        }
        done
    }

    fn edited(&mut self) {
        self.dirty = true;
        self.generation += 1;
    }

    pub fn scroll(&self) -> (u32, u32) {
        self.scroll
    }

    /// The room the frontend gave this pane, in pixels, and the size of one
    /// step. Re-clamps, so shrinking a window never leaves the crop pointing
    /// past the edge.
    pub fn set_viewport(&mut self, width: u32, height: u32, step: u32) {
        self.viewport = (width, height);
        self.step = step.max(1);
        self.clamp();
        self.follow_cursor();
    }

    /// `hjkl` and `Ctrl-E`/`Ctrl-Y` — by whole steps, counts multiplied in.
    pub fn step_by(&mut self, dx: i64, dy: i64) {
        self.by_pixels(dx * self.step as i64, dy * self.step as i64);
    }

    /// `Ctrl-D` / `Ctrl-U` — half the viewport at a time.
    pub fn half_page(&mut self, down: bool, count: usize) {
        let half = (self.viewport.1 / 2).max(self.step) as i64;
        let times = count.max(1) as i64;
        self.by_pixels(0, if down { half * times } else { -half * times });
    }

    /// `gg` and `G` — the closest thing a picture has to a first and last line.
    pub fn to_edge_y(&mut self, top: bool) {
        self.scroll.1 = if top { 0 } else { u32::MAX };
        self.clamp();
    }

    /// `0` and `$` — the closest thing it has to a column.
    pub fn to_edge_x(&mut self, left: bool) {
        self.scroll.0 = if left { 0 } else { u32::MAX };
        self.clamp();
    }

    fn by_pixels(&mut self, dx: i64, dy: i64) {
        self.scroll.0 = (self.scroll.0 as i64 + dx).max(0).min(u32::MAX as i64) as u32;
        self.scroll.1 = (self.scroll.1 as i64 + dy).max(0).min(u32::MAX as i64) as u32;
        self.clamp();
    }

    /// An image smaller than the viewport pins at zero — centering is the
    /// frontend's, but the clamp is what makes it stable.
    fn clamp(&mut self) {
        self.scroll.0 = self.scroll.0.min(self.width.saturating_sub(self.viewport.0));
        self.scroll.1 = self.scroll.1.min(self.height.saturating_sub(self.viewport.1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(width: u32, height: u32) -> Img {
        Img::from_pixels(PathBuf::from("t.png"), width, height, Vec::new(), 0)
    }

    #[test]
    fn steps_move_the_crop_and_clamp_at_both_ends() {
        let mut img = img(100, 100);
        img.set_viewport(40, 40, 10);

        img.step_by(1, 2);
        assert_eq!(img.scroll(), (10, 20));

        img.step_by(-9, -9);
        assert_eq!(img.scroll(), (0, 0), "clamped at the origin");

        img.step_by(99, 99);
        assert_eq!(img.scroll(), (60, 60), "clamped at image minus viewport");
    }

    #[test]
    fn the_edges_are_where_the_line_and_column_keys_go() {
        let mut img = img(100, 200);
        img.set_viewport(40, 40, 10);

        img.to_edge_y(false);
        img.to_edge_x(false);
        assert_eq!(img.scroll(), (60, 160));

        img.to_edge_y(true);
        img.to_edge_x(true);
        assert_eq!(img.scroll(), (0, 0));
    }

    #[test]
    fn half_a_page_is_half_the_viewport() {
        let mut img = img(100, 400);
        img.set_viewport(100, 100, 10);

        img.half_page(true, 1);
        assert_eq!(img.scroll(), (0, 50));

        img.half_page(true, 2);
        assert_eq!(img.scroll(), (0, 150));

        img.half_page(false, 1);
        assert_eq!(img.scroll(), (0, 100));
    }

    #[test]
    fn an_image_smaller_than_the_viewport_pins_at_zero() {
        let mut img = img(30, 30);
        img.set_viewport(40, 40, 10);

        img.step_by(5, 5);
        img.to_edge_y(false);

        assert_eq!(img.scroll(), (0, 0), "nothing to scroll toward");
    }

    #[test]
    fn shrinking_the_viewport_reclamps_an_existing_scroll() {
        let mut img = img(100, 100);
        img.set_viewport(40, 40, 10);
        img.to_edge_y(false);
        assert_eq!(img.scroll(), (0, 60));

        img.set_viewport(80, 80, 10);
        assert_eq!(img.scroll(), (0, 20), "the crop stays inside the image");
    }

    #[test]
    fn extensions_gate_the_attempt() {
        assert!(looks_like_image(Path::new("a/photo.PNG")));
        assert!(looks_like_image(Path::new("photo.webp")));
        assert!(!looks_like_image(Path::new("photo.rs")));
        assert!(!looks_like_image(Path::new("png")));
    }

    /// A real decode round-trip, through a file the test writes itself.
    #[test]
    fn a_png_on_disk_decodes_to_its_pixels() {
        let dir = std::env::temp_dir().join(format!("bi-img-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.png");
        let buffer = image::RgbaImage::from_pixel(3, 2, image::Rgba([1, 2, 3, 255]));
        buffer.save(&path).unwrap();

        let img = Img::open(&path, 7).unwrap();
        assert_eq!((img.width, img.height), (3, 2));
        assert_eq!(img.rgba.len(), 3 * 2 * 4);
        assert_eq!(img.id, 7);

        std::fs::write(&path, "not a png").unwrap();
        assert!(Img::open(&path, 8).is_err(), "corrupt bytes are an error, not a panic");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn zoom_clamps_doubles_halves_and_resets() {
        let mut img = img(100, 100);
        assert_eq!(img.zoom(), 1.0);
        img.zoom_step(true);
        img.zoom_step(true);
        assert_eq!(img.zoom(), 4.0);
        img.zoom_step(false);
        assert_eq!(img.zoom(), 2.0);
        img.set_zoom(1000.0);
        assert_eq!(img.zoom(), 32.0, "clamped high");
        img.set_zoom(0.0);
        assert_eq!(img.zoom(), 0.05, "clamped low");
        img.set_zoom(0.1);
        assert_eq!(img.zoom(), 0.1);
    }
}
