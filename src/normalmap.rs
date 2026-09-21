//! Height map in, normal map out: the pipeline behind
//! `:set editor normalmap`, as pure functions, and the form of knobs that
//! drives it. See `docs/specs/normalmap.md`.

use crate::form::{Field, Form};

/// Where the height comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Source {
    #[default]
    Luma,
    R,
    G,
    B,
    A,
}

impl Source {
    const NAMES: [&str; 5] = ["luma", "r", "g", "b", "a"];

    fn parse(s: &str) -> Self {
        match s {
            "r" => Self::R,
            "g" => Self::G,
            "b" => Self::B,
            "a" => Self::A,
            _ => Self::Luma,
        }
    }
}

/// The 3×3 gradient filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    #[default]
    Sobel,
    Scharr,
    Prewitt,
}

impl Filter {
    const NAMES: [&str; 3] = ["sobel", "scharr", "prewitt"];

    fn parse(s: &str) -> Self {
        match s {
            "scharr" => Self::Scharr,
            "prewitt" => Self::Prewitt,
            _ => Self::Sobel,
        }
    }

    /// The horizontal kernel's weights for the left and right columns
    /// (top, middle, bottom), and the sum that normalises a unit slope to
    /// a gradient of one per pixel.
    fn weights(self) -> ([f32; 3], f32) {
        match self {
            Self::Sobel => ([1.0, 2.0, 1.0], 8.0),
            Self::Scharr => ([3.0, 10.0, 3.0], 32.0),
            Self::Prewitt => ([1.0, 1.0, 1.0], 6.0),
        }
    }
}

/// Which output the result window shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Map {
    #[default]
    Normal,
    Displacement,
}

impl Map {
    pub const ALL: [Map; 2] = [Map::Normal, Map::Displacement];
    const NAMES: [&str; 2] = ["normal", "displacement"];

    /// `atlas_<suffix>.png`.
    pub fn suffix(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Displacement => "disp",
        }
    }

    pub fn from_form(form: &Form) -> Self {
        match form.get_choice("map") {
            "displacement" => Self::Displacement,
            _ => Self::Normal,
        }
    }
}

/// The knobs.
#[derive(Debug, Clone, PartialEq)]
pub struct Params {
    /// Slope scale.
    pub strength: f32,
    /// Gamma on the height.
    pub level: f32,
    /// Gaussian sigma on the height, pixels; zero is none.
    pub blur: f32,
    pub filter: Filter,
    pub source: Source,
    pub invert_r: bool,
    pub invert_g: bool,
    pub invert_height: bool,
    /// Blue encodes `z` over -1..1 like the other two; else `z` itself.
    pub z_full: bool,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            strength: 2.5,
            level: 1.0,
            blur: 0.0,
            filter: Filter::Sobel,
            source: Source::Luma,
            invert_r: false,
            invert_g: false,
            invert_height: false,
            z_full: true,
        }
    }
}

impl Params {
    pub fn from_form(form: &Form) -> Self {
        Self {
            strength: form.get_f32("strength"),
            level: form.get_f32("level"),
            blur: form.get_i64("blur") as f32,
            filter: Filter::parse(form.get_choice("filter")),
            source: Source::parse(form.get_choice("height")),
            invert_r: form.get_bool("invert_r"),
            invert_g: form.get_bool("invert_g"),
            invert_height: form.get_bool("invert_h"),
            z_full: form.get_choice("zrange") != "0..1",
        }
    }
}

/// The form the tool fills — its fields are `Params` spelled for a pane,
/// with `map` first so `Tab` has something to cycle.
pub fn form() -> Form {
    let d = Params::default();
    let mut form = Form::with_cycle("normalmap", "Normal map", "map");
    form.push(Field::choice("map", "Map", &Map::NAMES, 0));
    form.push(Field::float("strength", "Strength", 0.1, 20.0, 0.1, d.strength));
    form.push(Field::float("level", "Level", 0.1, 10.0, 0.1, d.level));
    form.push(Field::int("blur", "Blur", 0, 32, 1, d.blur as i64));
    form.push(Field::choice("filter", "Filter", &Filter::NAMES, 0));
    form.push(Field::choice("height", "Height", &Source::NAMES, 0));
    form.push(Field::bool("invert_r", "Invert R", d.invert_r));
    form.push(Field::bool("invert_g", "Invert G", d.invert_g));
    form.push(Field::bool("invert_h", "Invert H", d.invert_height));
    form.push(Field::choice("zrange", "Z range", &["-1..1", "0..1"], 0));
    form
}

/// The height, `0..1` per pixel: from the source channel, inverted if
/// asked, blurred, then raised to the level.
pub fn height(rgba: &[u8], width: u32, height: u32, p: &Params) -> Vec<f32> {
    let mut h: Vec<f32> = rgba
        .chunks_exact(4)
        .map(|px| {
            let v = match p.source {
                Source::Luma => 0.299 * px[0] as f32 + 0.587 * px[1] as f32 + 0.114 * px[2] as f32,
                Source::R => px[0] as f32,
                Source::G => px[1] as f32,
                Source::B => px[2] as f32,
                Source::A => px[3] as f32,
            } / 255.0;
            if p.invert_height { 1.0 - v } else { v }
        })
        .collect();
    if p.blur > 0.0 {
        h = gaussian(&h, width, height, p.blur);
    }
    if p.level != 1.0 {
        for v in &mut h {
            *v = v.max(0.0).powf(p.level);
        }
    }
    h
}

/// A separable Gaussian over a float field, edges clamped.
fn gaussian(field: &[f32], width: u32, height: u32, sigma: f32) -> Vec<f32> {
    let radius = (sigma * 3.0).ceil() as i64;
    let kernel: Vec<f32> =
        (-radius..=radius).map(|i| (-(i * i) as f32 / (2.0 * sigma * sigma)).exp()).collect();
    let sum: f32 = kernel.iter().sum();
    let (w, h) = (width as i64, height as i64);
    let at = |f: &[f32], x: i64, y: i64| f[(y.clamp(0, h - 1) * w + x.clamp(0, w - 1)) as usize];
    let mut rows = vec![0.0; field.len()];
    for y in 0..h {
        for x in 0..w {
            let v: f32 = kernel
                .iter()
                .enumerate()
                .map(|(k, kw)| kw * at(field, x + k as i64 - radius, y))
                .sum();
            rows[(y * w + x) as usize] = v / sum;
        }
    }
    let mut out = vec![0.0; field.len()];
    for y in 0..h {
        for x in 0..w {
            let v: f32 = kernel
                .iter()
                .enumerate()
                .map(|(k, kw)| kw * at(&rows, x, y + k as i64 - radius))
                .sum();
            out[(y * w + x) as usize] = v / sum;
        }
    }
    out
}

/// The normal map, RGBA, `rgb = n·0.5 + 0.5` — blue as `z` itself when
/// the Z range is `0..1`.
pub fn normal(rgba: &[u8], width: u32, height_px: u32, p: &Params) -> Vec<u8> {
    let h = height(rgba, width, height_px, p);
    let (w, hh) = (width as i64, height_px as i64);
    let at = |x: i64, y: i64| h[(y.clamp(0, hh - 1) * w + x.clamp(0, w - 1)) as usize];
    let ([a, b, c], norm) = p.filter.weights();
    let mut out = Vec::with_capacity(rgba.len());
    for y in 0..hh {
        for x in 0..w {
            let dx = (a * (at(x + 1, y - 1) - at(x - 1, y - 1))
                + b * (at(x + 1, y) - at(x - 1, y))
                + c * (at(x + 1, y + 1) - at(x - 1, y + 1)))
                / norm;
            let dy = (a * (at(x - 1, y + 1) - at(x - 1, y - 1))
                + b * (at(x, y + 1) - at(x, y - 1))
                + c * (at(x + 1, y + 1) - at(x + 1, y - 1)))
                / norm;
            let (mut nx, mut ny, nz) = (-dx * p.strength, -dy * p.strength, 1.0f32);
            let len = (nx * nx + ny * ny + nz * nz).sqrt();
            let (nx2, ny2, nz2) = (nx / len, ny / len, nz / len);
            nx = if p.invert_r { -nx2 } else { nx2 };
            ny = if p.invert_g { -ny2 } else { ny2 };
            let enc = |v: f32| ((v * 0.5 + 0.5) * 255.0).round().clamp(0.0, 255.0) as u8;
            let blue =
                if p.z_full { enc(nz2) } else { (nz2 * 255.0).round().clamp(0.0, 255.0) as u8 };
            out.extend_from_slice(&[enc(nx), enc(ny), blue, 255]);
        }
    }
    out
}

/// The levelled height as grey, opaque.
pub fn displacement(rgba: &[u8], width: u32, height_px: u32, p: &Params) -> Vec<u8> {
    height(rgba, width, height_px, p)
        .into_iter()
        .flat_map(|v| {
            let g = (v * 255.0).round().clamp(0.0, 255.0) as u8;
            [g, g, g, 255]
        })
        .collect()
}

/// One map, rendered.
pub fn render(map: Map, rgba: &[u8], width: u32, height_px: u32, p: &Params) -> Vec<u8> {
    match map {
        Map::Normal => normal(rgba, width, height_px, p),
        Map::Displacement => displacement(rgba, width, height_px, p),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `w`×1 grey ramp, black to white, opaque.
    fn ramp(w: u32) -> Vec<u8> {
        (0..w)
            .flat_map(|x| {
                let v = (x * 255 / (w - 1)) as u8;
                [v, v, v, 255]
            })
            .collect()
    }

    fn flat(w: u32, h: u32, v: u8) -> Vec<u8> {
        (0..w * h).flat_map(|_| [v, v, v, 255]).collect()
    }

    #[test]
    fn height_is_luma_or_a_channel_and_inverts() {
        let px = ramp(5);
        let p = Params::default();
        let h = height(&px, 5, 1, &p);
        assert_eq!(h.len(), 5);
        assert!((h[0] - 0.0).abs() < 1e-6);
        assert!((h[4] - 1.0).abs() < 1e-6);
        assert!(h[1] < h[2] && h[2] < h[3]);

        let red: Vec<u8> = [[200, 0, 0, 255], [0, 200, 0, 255]].concat();
        let p = Params { source: Source::R, ..Params::default() };
        let h = height(&red, 2, 1, &p);
        assert!((h[0] - 200.0 / 255.0).abs() < 1e-6);
        assert!((h[1] - 0.0).abs() < 1e-6);

        let p = Params { invert_height: true, ..Params::default() };
        let h = height(&px, 5, 1, &p);
        assert!((h[0] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn level_is_a_gamma_on_the_height() {
        let px = ramp(3);
        let p = Params { level: 2.0, ..Params::default() };
        let h = height(&px, 3, 1, &p);
        assert!((h[1] - 0.25).abs() < 0.01, "{h:?}");
    }

    #[test]
    fn a_flat_picture_is_a_flat_normal_map() {
        let px = flat(4, 4, 100);
        let out = normal(&px, 4, 4, &Params::default());
        assert_eq!(out.len(), 4 * 4 * 4);
        for p in out.chunks(4) {
            assert_eq!(p, &[128, 128, 255, 255], "{p:?}");
        }
        let p = Params { z_full: false, ..Params::default() };
        let out = normal(&px, 4, 4, &p);
        assert_eq!(&out[0..4], &[128, 128, 255, 255], "z is 1 either way");
    }

    #[test]
    fn a_ramp_leans_and_invert_r_leans_it_the_other_way() {
        let mut px = Vec::new();
        for _ in 0..3 {
            px.extend(ramp(8));
        }
        let p = Params::default();
        let out = normal(&px, 8, 3, &p);
        let mid = &out[(8 + 4) * 4..(8 + 4) * 4 + 4];
        assert!(mid[0] < 128, "height rises to the right, so the normal leans left: {mid:?}");
        assert_eq!(mid[1], 128, "no slope along y");
        assert!(mid[2] < 255 && mid[2] > 128);

        let p = Params { invert_r: true, ..Params::default() };
        let out = normal(&px, 8, 3, &p);
        let mid = &out[(8 + 4) * 4..(8 + 4) * 4 + 4];
        assert!(mid[0] > 128, "{mid:?}");

        let p = Params { strength: 0.1, ..Params::default() };
        let out = normal(&px, 8, 3, &p);
        let mid = &out[(8 + 4) * 4..(8 + 4) * 4 + 4];
        assert!(mid[0] >= 120 && mid[2] >= 250, "nearly flat: {mid:?}");

        for filter in [Filter::Sobel, Filter::Scharr, Filter::Prewitt] {
            let p = Params { filter, ..Params::default() };
            let out = normal(&px, 8, 3, &p);
            assert!(out[(8 + 4) * 4] < 128, "{filter:?}");
        }
    }

    #[test]
    fn displacement_is_the_levelled_height_as_grey() {
        let px = ramp(3);
        let out = displacement(&px, 3, 1, &Params::default());
        assert_eq!(&out[0..4], &[0, 0, 0, 255]);
        assert_eq!(&out[8..12], &[255, 255, 255, 255]);
        let p = Params { level: 2.0, ..Params::default() };
        let out = displacement(&px, 3, 1, &p);
        assert!(out[4] < 70, "{}", out[4]);
    }

    #[test]
    fn the_form_round_trips_into_params_and_names_the_map() {
        let mut f = form();
        assert_eq!(Map::from_form(&f), Map::Normal);
        assert_eq!(Params::from_form(&f), Params::default());
        f.set("strength", "4").unwrap();
        f.set("filter", "scharr").unwrap();
        f.set("invert_g", "true").unwrap();
        f.set("zrange", "0..1").unwrap();
        f.set("height", "b").unwrap();
        f.set("blur", "3").unwrap();
        let p = Params::from_form(&f);
        assert_eq!(p.strength, 4.0);
        assert_eq!(p.filter, Filter::Scharr);
        assert!(p.invert_g);
        assert!(!p.z_full);
        assert_eq!(p.source, Source::B);
        assert_eq!(p.blur, 3.0);
        assert!(f.cycle_map());
        assert_eq!(Map::from_form(&f), Map::Displacement);
        assert_eq!(Map::Displacement.suffix(), "disp");
        assert_eq!(Map::Normal.suffix(), "normal");
    }

    #[test]
    fn blur_smooths_the_height_without_changing_its_size() {
        let mut px = Vec::new();
        for _ in 0..4 {
            px.extend(
                [[0, 0, 0, 255], [255, 255, 255, 255], [0, 0, 0, 255], [255, 255, 255, 255]]
                    .concat(),
            );
        }
        let sharp = height(&px, 4, 4, &Params::default());
        let p = Params { blur: 1.0, ..Params::default() };
        let soft = height(&px, 4, 4, &p);
        assert_eq!(soft.len(), 16);
        assert!(soft[5] < sharp[5], "a white pixel dims beside black ones");
        assert!(soft[4] > sharp[4]);
    }
}
