//! Whole-image operations and the format an image is written in.
//!
//! Functions from pixels to pixels over the `image` crate's `imageops`,
//! with no editor in them; `Img` calls one and records the result as an
//! undo step. See `docs/specs/image-ops.md`.

use std::path::Path;

use image::{RgbaImage, imageops};

/// RGBA8 pixels as the `image` crate's buffer, for one operation.
fn buffer(rgba: &[u8], width: u32, height: u32) -> RgbaImage {
    RgbaImage::from_raw(width, height, rgba.to_vec()).expect("width × height × 4 bytes")
}

/// Every channel the luma, alpha untouched.
pub fn grayscale(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    // Per pixel, so the dimensions are only a shape check; kept in the
    // signature so every operation reads the same way.
    debug_assert_eq!(rgba.len() as u32, width * height * 4);
    let mut out = rgba.to_vec();
    for px in out.chunks_exact_mut(4) {
        // Rec. 601 luma, the `image` crate's own weights.
        let y = (0.299 * px[0] as f32 + 0.587 * px[1] as f32 + 0.114 * px[2] as f32).round() as u8;
        px[0] = y;
        px[1] = y;
        px[2] = y;
    }
    out
}

/// Colours inverted, alpha untouched.
pub fn invert(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    debug_assert_eq!(rgba.len() as u32, width * height * 4);
    let mut out = rgba.to_vec();
    for px in out.chunks_exact_mut(4) {
        px[0] = 255 - px[0];
        px[1] = 255 - px[1];
        px[2] = 255 - px[2];
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// Left-right: columns mirrored.
    X,
    /// Top-bottom: rows mirrored.
    Y,
}

impl Axis {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "x" => Some(Self::X),
            "y" => Some(Self::Y),
            _ => None,
        }
    }
}

pub fn flip(rgba: &[u8], width: u32, height: u32, axis: Axis) -> Vec<u8> {
    let img = buffer(rgba, width, height);
    match axis {
        Axis::X => imageops::flip_horizontal(&img),
        Axis::Y => imageops::flip_vertical(&img),
    }
    .into_raw()
}

/// A Gaussian blur by `sigma` pixels.
pub fn blur(rgba: &[u8], width: u32, height: u32, sigma: f32) -> Vec<u8> {
    imageops::blur(&buffer(rgba, width, height), sigma).into_raw()
}

/// How `resize` samples. Nearest is the default: the pictures this
/// editor is for are sheets and sprites, where a filter that blends is a
/// filter that ruins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    #[default]
    Nearest,
    Bilinear,
    Lanczos,
}

impl Filter {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "nearest" => Some(Self::Nearest),
            "bilinear" => Some(Self::Bilinear),
            "lanczos" => Some(Self::Lanczos),
            _ => None,
        }
    }

    fn kind(self) -> imageops::FilterType {
        match self {
            Self::Nearest => imageops::FilterType::Nearest,
            Self::Bilinear => imageops::FilterType::Triangle,
            Self::Lanczos => imageops::FilterType::Lanczos3,
        }
    }
}

/// The picture at `size` pixels. Returns the pixels and their size.
pub fn resize(
    rgba: &[u8],
    width: u32,
    height: u32,
    size: (u32, u32),
    filter: Filter,
) -> (Vec<u8>, u32, u32) {
    let out = imageops::resize(&buffer(rgba, width, height), size.0, size.1, filter.kind());
    (out.into_raw(), size.0, size.1)
}

/// The `x, y, w, h` rectangle, refused rather than clipped when it runs
/// past the picture.
pub fn crop(
    rgba: &[u8],
    width: u32,
    height: u32,
    (x, y, w, h): (u32, u32, u32, u32),
) -> Result<(Vec<u8>, u32, u32), String> {
    if w == 0 || h == 0 || x + w > width || y + h > height {
        return Err(format!("{x},{y},{w},{h} runs past {width}×{height}"));
    }
    let out = imageops::crop_imm(&buffer(rgba, width, height), x, y, w, h).to_image();
    Ok((out.into_raw(), w, h))
}

/// What an image can be written as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Png,
    Jpg,
    Webp,
    Bmp,
}

impl Kind {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "png" => Some(Self::Png),
            "jpg" | "jpeg" => Some(Self::Jpg),
            "webp" => Some(Self::Webp),
            "bmp" => Some(Self::Bmp),
            _ => None,
        }
    }

    /// The extension a path gets, and the name the status row uses.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpg => "jpg",
            Self::Webp => "webp",
            Self::Bmp => "bmp",
        }
    }
}

/// How `:w` encodes: the kind, whether alpha goes with it, and the JPEG
/// quality. Set from the extension when a file opens, changed by
/// `:tool image conv`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Format {
    pub kind: Kind,
    /// Written with an alpha channel. JPEG has none and ignores this.
    pub alpha: bool,
    /// 1..=100, JPEG only.
    pub quality: u8,
}

impl Default for Format {
    fn default() -> Self {
        Self { kind: Kind::Png, alpha: true, quality: 90 }
    }
}

impl Format {
    /// The format a path's extension names, with the defaults for the
    /// rest. `None` for an extension nothing here can write.
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?;
        Some(Self { kind: Kind::parse(ext)?, ..Self::default() })
    }

    /// What `from_path` says when it says `None`.
    pub fn refusal(ext: &str) -> String {
        format!("cannot write {ext} (png, jpg, webp, bmp)")
    }

    /// `path` with this format's extension.
    pub fn repoint(&self, path: &Path) -> std::path::PathBuf {
        path.with_extension(self.kind.extension())
    }

    /// Encodes and writes.
    pub fn write(&self, path: &Path, rgba: &[u8], width: u32, height: u32) -> Result<(), String> {
        let err = |e: image::ImageError| format!("error: {e}");
        let img = buffer(rgba, width, height);
        let rgb = || image::DynamicImage::ImageRgba8(img.clone()).to_rgb8();
        match self.kind {
            Kind::Jpg => {
                let file = std::fs::File::create(path).map_err(|e| format!("error: {e}"))?;
                let writer = std::io::BufWriter::new(file);
                let encoder =
                    image::codecs::jpeg::JpegEncoder::new_with_quality(writer, self.quality);
                image::ImageEncoder::write_image(
                    encoder,
                    rgb().as_raw(),
                    width,
                    height,
                    image::ExtendedColorType::Rgb8,
                )
                .map_err(err)
            }
            _ if self.alpha => img.save(path).map_err(err),
            _ => rgb().save(path).map_err(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(w: u32, h: u32, colours: &[[u8; 4]]) -> Vec<u8> {
        assert_eq!(colours.len() as u32, w * h);
        colours.concat()
    }

    const A: [u8; 4] = [200, 100, 0, 255];
    const B: [u8; 4] = [0, 100, 200, 255];
    const C: [u8; 4] = [50, 50, 50, 128];
    const D: [u8; 4] = [255, 255, 255, 255];

    #[test]
    fn grayscale_equalises_the_channels_and_keeps_alpha() {
        let out = grayscale(&px(2, 1, &[A, C]), 2, 1);
        assert_eq!(out[0], out[1]);
        assert_eq!(out[1], out[2]);
        assert_eq!(out[3], 255);
        assert_eq!(out[7], 128, "alpha is not a colour");
    }

    #[test]
    fn invert_twice_is_the_image_it_was() {
        let src = px(2, 2, &[A, B, C, D]);
        let once = invert(&src, 2, 2);
        assert_eq!(&once[0..3], &[55, 155, 255]);
        assert_eq!(once[3], 255, "alpha stays");
        assert_eq!(invert(&once, 2, 2), src);
    }

    #[test]
    fn flip_mirrors_columns_or_rows() {
        let src = px(2, 2, &[A, B, C, D]);
        assert_eq!(flip(&src, 2, 2, Axis::X), px(2, 2, &[B, A, D, C]));
        assert_eq!(flip(&src, 2, 2, Axis::Y), px(2, 2, &[C, D, A, B]));
    }

    #[test]
    fn resize_nearest_makes_blocks_and_lanczos_is_a_filter() {
        let src = px(2, 2, &[A, B, C, D]);
        let (out, w, h) = resize(&src, 2, 2, (4, 4), Filter::Nearest);
        assert_eq!((w, h), (4, 4));
        assert_eq!(&out[0..4], &A);
        assert_eq!(&out[4..8], &A);
        assert_eq!(&out[8..12], &B);
        let (out, w, h) = resize(&src, 2, 2, (1, 1), Filter::Lanczos);
        assert_eq!((w, h), (1, 1));
        assert_eq!(out.len(), 4);

        assert_eq!(Filter::parse("nearest"), Some(Filter::Nearest));
        assert_eq!(Filter::parse("bilinear"), Some(Filter::Bilinear));
        assert_eq!(Filter::parse("lanczos"), Some(Filter::Lanczos));
        assert_eq!(Filter::parse("cubic"), None);
    }

    #[test]
    fn crop_takes_the_rectangle_and_refuses_one_past_the_edge() {
        let src = px(2, 2, &[A, B, C, D]);
        assert_eq!(crop(&src, 2, 2, (1, 1, 1, 1)), Ok((D.to_vec(), 1, 1)));
        assert_eq!(crop(&src, 2, 2, (1, 0, 2, 1)), Err("1,0,2,1 runs past 2×2".into()));
        assert!(crop(&src, 2, 2, (0, 0, 0, 1)).is_err(), "nothing is not a crop");
    }

    #[test]
    fn blur_keeps_the_size() {
        let src = px(2, 2, &[A, B, C, D]);
        assert_eq!(blur(&src, 2, 2, 1.0).len(), 16);
    }

    #[test]
    fn the_format_comes_from_the_extension_and_says_what_it_can_write() {
        assert_eq!(
            Format::from_path(std::path::Path::new("a.png")).map(|f| f.kind),
            Some(Kind::Png)
        );
        assert_eq!(
            Format::from_path(std::path::Path::new("a.JPEG")).map(|f| f.kind),
            Some(Kind::Jpg)
        );
        assert_eq!(
            Format::from_path(std::path::Path::new("a.webp")).map(|f| f.kind),
            Some(Kind::Webp)
        );
        assert_eq!(
            Format::from_path(std::path::Path::new("a.bmp")).map(|f| f.kind),
            Some(Kind::Bmp)
        );
        assert_eq!(Format::from_path(std::path::Path::new("a.gif")), None);
        assert_eq!(Format::refusal("gif"), "cannot write gif (png, jpg, webp, bmp)");
        assert_eq!(Kind::Jpg.extension(), "jpg");
        assert_eq!(Format::default().quality, 90);
        assert!(Format::default().alpha);
    }

    /// A round trip through every writable encoder, and jpg drops alpha
    /// without being asked.
    #[test]
    fn every_kind_encodes_to_a_file_that_decodes() {
        let dir = std::env::temp_dir().join(format!("bi-imgops-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = px(2, 2, &[A, B, C, D]);
        for kind in [Kind::Png, Kind::Jpg, Kind::Webp, Kind::Bmp] {
            let path = dir.join(format!("t.{}", kind.extension()));
            let format = Format { kind, ..Format::default() };
            format.write(&path, &src, 2, 2).unwrap();
            let back = image::open(&path).unwrap();
            assert_eq!((back.width(), back.height()), (2, 2), "{kind:?}");
        }
        let path = dir.join("noalpha.png");
        Format { kind: Kind::Png, alpha: false, quality: 90 }.write(&path, &src, 2, 2).unwrap();
        assert!(!image::open(&path).unwrap().color().has_alpha());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
