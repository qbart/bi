//! An opened image: pixels, dimensions, and a crop position.
//!
//! State only, like [`crate::tree::Tree`] — no terminal, no escape sequences.
//! The core decodes because pixel dimensions are core facts (the status line
//! and the scroll bounds are made of them) and because every frontend wants
//! the same bytes: a GUI blits `rgba`, the terminal frontend encodes it for
//! the wire. See `docs/specs/images.md`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

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
}

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
        Self { path, width, height, rgba, scroll: (0, 0), viewport: (0, 0), step: 1, id }
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
}
