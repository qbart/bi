//! Pixels for a tool's picture: the curve's plot, the colour picker,
//! the gradient's bar. An RGBA canvas with the few primitives they draw
//! with — lines, discs, rectangles, a checkerboard for alpha, a tiny
//! font for labels. No editor in it.

/// The ground every picture starts as.
pub const BG: [u8; 4] = [24, 24, 28, 255];

/// Three-by-five glyphs for the tick labels: digits, minus, point.
fn glyph(c: char) -> [u8; 5] {
    match c {
        '0' => [0b111, 0b101, 0b101, 0b101, 0b111],
        '1' => [0b010, 0b110, 0b010, 0b010, 0b111],
        '2' => [0b111, 0b001, 0b111, 0b100, 0b111],
        '3' => [0b111, 0b001, 0b111, 0b001, 0b111],
        '4' => [0b101, 0b101, 0b111, 0b001, 0b001],
        '5' => [0b111, 0b100, 0b111, 0b001, 0b111],
        '6' => [0b111, 0b100, 0b111, 0b101, 0b111],
        '7' => [0b111, 0b001, 0b001, 0b001, 0b001],
        '8' => [0b111, 0b101, 0b111, 0b101, 0b111],
        '9' => [0b111, 0b101, 0b111, 0b001, 0b111],
        '-' => [0b000, 0b000, 0b111, 0b000, 0b000],
        '.' => [0b000, 0b000, 0b000, 0b000, 0b010],
        _ => [0; 5],
    }
}

pub struct Canvas {
    w: u32,
    h: u32,
    px: Vec<u8>,
}

impl Canvas {
    pub fn new(w: u32, h: u32) -> Self {
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..w * h {
            px.extend_from_slice(&BG);
        }
        Self { w, h, px }
    }

    pub fn put(&mut self, x: i64, y: i64, c: [u8; 4]) {
        if x < 0 || y < 0 || x >= self.w as i64 || y >= self.h as i64 {
            return;
        }
        let at = ((y as u32 * self.w + x as u32) * 4) as usize;
        self.px[at..at + 4].copy_from_slice(&c);
    }

    pub fn line(&mut self, x0: i64, y0: i64, x1: i64, y1: i64, c: [u8; 4]) {
        let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
        let (sx, sy) = (if x0 < x1 { 1 } else { -1 }, if y0 < y1 { 1 } else { -1 });
        let (mut x, mut y, mut err) = (x0, y0, dx + dy);
        loop {
            self.put(x, y, c);
            if x == x1 && y == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                err += dx;
                y += sy;
            }
        }
    }

    /// A filled rectangle from `x, y`, `w` by `h`.
    pub fn rect(&mut self, x: i64, y: i64, w: i64, h: i64, c: [u8; 4]) {
        for row in y..y + h {
            for col in x..x + w {
                self.put(col, row, c);
            }
        }
    }

    /// The rectangle's outline, `thick` pixels in from its edge.
    pub fn frame(&mut self, x: i64, y: i64, w: i64, h: i64, thick: i64, c: [u8; 4]) {
        for i in 0..thick {
            self.line(x + i, y + i, x + w - 1 - i, y + i, c);
            self.line(x + i, y + h - 1 - i, x + w - 1 - i, y + h - 1 - i, c);
            self.line(x + i, y + i, x + i, y + h - 1 - i, c);
            self.line(x + w - 1 - i, y + i, x + w - 1 - i, y + h - 1 - i, c);
        }
    }

    /// A grey checkerboard, `cell` pixels a square — what shows through
    /// a transparent colour.
    pub fn checker(&mut self, x: i64, y: i64, w: i64, h: i64, cell: i64) {
        for row in y..y + h {
            for col in x..x + w {
                let dark = ((col - x) / cell + (row - y) / cell) % 2 == 0;
                self.put(col, row, if dark { [96, 96, 96, 255] } else { [160, 160, 160, 255] });
            }
        }
    }

    /// `c` laid over the pixel by its alpha; the pixel stays opaque.
    pub fn blend(&mut self, x: i64, y: i64, c: [u8; 4]) {
        if x < 0 || y < 0 || x >= self.w as i64 || y >= self.h as i64 {
            return;
        }
        let at = ((y as u32 * self.w + x as u32) * 4) as usize;
        let a = c[3] as f32 / 255.0;
        for (px, &over) in self.px[at..at + 3].iter_mut().zip(&c[..3]) {
            let under = *px as f32;
            *px = (over as f32 * a + under * (1.0 - a)).round() as u8;
        }
        self.px[at + 3] = 255;
    }

    pub fn size(&self) -> (u32, u32) {
        (self.w, self.h)
    }

    pub fn into_pixels(self) -> Vec<u8> {
        self.px
    }

    pub fn disc(&mut self, cx: i64, cy: i64, r: i64, c: [u8; 4]) {
        for y in -r..=r {
            for x in -r..=r {
                if x * x + y * y <= r * r {
                    self.put(cx + x, cy + y, c);
                }
            }
        }
    }

    /// `text` in the tiny font with its top-left at `x, y`; four pixels
    /// per glyph. Returns the width drawn.
    pub fn text(&mut self, x: i64, y: i64, text: &str, c: [u8; 4]) -> i64 {
        let mut at = x;
        for ch in text.chars() {
            let rows = glyph(ch);
            for (r, bits) in rows.iter().enumerate() {
                for col in 0..3 {
                    if bits & (0b100 >> col) != 0 {
                        self.put(at + col, y + r as i64, c);
                    }
                }
            }
            at += 4;
        }
        at - x
    }
}

pub const fn text_width(s: &str) -> i64 {
    s.len() as i64 * 4
}
