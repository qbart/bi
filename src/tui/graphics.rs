//! The kitty graphics protocol: detection, uploads, placements.
//!
//! Everything that knows what the protocol looks like lives here. `render`
//! decides *what* goes where and says so as [`Place`]s; this module diffs
//! them against what is on screen and writes the escapes. ratatui never
//! learns images exist, which is what keeps the next backend possible.
//!
//! See `docs/specs/images.md`.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};

use base64::Engine as _;
use bi::editor::Editor;

/// The id the tilemap's cursor frame is uploaded under — the frontend's
/// own, and one the core's counter, which starts at one and climbs, never
/// reaches. See `docs/specs/tilemap.md`.
pub const FRAME_ID: u64 = u64::MAX;

/// How, if at all, pixels reach the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    None,
    /// The terminal itself answered the handshake.
    Direct,
    /// Inside tmux, with `allow-passthrough` on and a capable terminal
    /// outside: every escape rides tmux's passthrough wrapper.
    Tmux,
}

/// One image at one spot, cropped — what `render` wants on screen this
/// frame. Cells for the destination, pixels for the crop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Place {
    /// The image's core id — `bi::img::Img::id`.
    pub id: u64,
    /// The kitty placement id — the window's id, made nonzero. Not a
    /// constant: a bare `:vs` clones the window, image and all, and two
    /// placements of one image under one placement id are one placement.
    pub pid: u32,
    /// Destination, in cells, screen-absolute.
    pub col: u16,
    pub row: u16,
    /// The cells the crop covers, for the overlap test against overlays.
    pub cols: u16,
    pub rows: u16,
    /// x, y, width, height of the source rectangle, in pixels.
    pub crop: (u32, u32, u32, u32),
    /// Pixel offset of the crop's top-left within the destination cell —
    /// the protocol's `X`/`Y`. Zero for an image, which starts on a cell;
    /// the tile frame lands wherever the tile does.
    pub offset: (u16, u16),
    /// `-1` for a picture, under text glyphs and above background fills;
    /// `0` for the frame that rides over the picture.
    pub z: i32,
    /// For the tile frame: the whole frame's size, which the crop may show
    /// only part of. Zero for a picture.
    pub frame: (u32, u32),
    /// The zoom the uploaded pixels are scaled by — `f32` bits, so the
    /// struct stays `Eq`. `1.0` for the original; the crisp path uploads a
    /// scaled copy and says which. See `docs/specs/zoom.md`.
    pub scale: u32,
    /// Terminal-scaled: the crop is asked to fill `cols`×`rows` cells
    /// (`c=`/`r=`) rather than drawn at native size. The path past the
    /// budget.
    pub fit: bool,
}

impl Place {
    /// The zoom of the pixels this placement draws from.
    pub fn zoom(&self) -> f32 {
        f32::from_bits(self.scale)
    }

    /// The tile frame's full size. The crop's width and height are the
    /// *visible* part, so the whole is kept beside them for the upload.
    pub fn frame_size(&self) -> (u32, u32) {
        self.frame
    }

    /// Whether any of this placement's cells fall inside `x, y, w, h`.
    pub fn intersects(&self, x: u16, y: u16, w: u16, h: u16) -> bool {
        self.col < x + w && x < self.col + self.cols && self.row < y + h && y < self.row + self.rows
    }
}

/// What the terminal is currently showing, and what it already holds.
pub struct Graphics {
    support: Support,
    /// Ids whose pixels the terminal has been sent, and at which generation.
    /// Pixels go up once per generation; every frame after moves a
    /// placement, which is metadata. An edit moves the generation, and the
    /// pixels go up again under the same id.
    sent: HashMap<u64, u64>,
    /// The placements on screen, by image and placement id. Re-creating a
    /// placement under the same placement id replaces it atomically, so only
    /// what moved is ever rewritten — which is what keeps scrolling
    /// flicker-free.
    placed: HashMap<(u64, u32), Place>,
    /// One cell in pixels, asked of tmux once at startup — the pane's own
    /// pty does not always carry pixel sizes, and the client's cell size is
    /// the honest substitute. `None` outside tmux, and inside one whose
    /// tmux is too old to say.
    tmux_cell: Option<(u16, u16)>,
}

impl Graphics {
    pub fn new(support: Support) -> Self {
        let tmux_cell = match support {
            Support::Tmux => {
                tmux_out(&["display-message", "-p", "#{client_cell_width},#{client_cell_height}"])
                    .and_then(|s| parse_pair(s.trim()))
                    .filter(|&(w, h)| w > 0 && h > 0)
            }
            _ => None,
        };
        Self { support, sent: HashMap::new(), placed: HashMap::new(), tmux_cell }
    }

    /// One cell's size in pixels, when the terminal both draws images and
    /// says how big its cells are. Asked per frame — a font change
    /// mid-session is legal and TIOCGWINSZ is one cheap ioctl. `None` means
    /// the placeholder path: no arithmetic on a guess.
    pub fn cell_size(&self) -> Option<(u16, u16)> {
        if self.support == Support::None {
            return None;
        }
        if let Ok(ws) = ratatui::crossterm::terminal::window_size()
            && ws.columns > 0
            && ws.rows > 0
            && ws.width > 0
            && ws.height > 0
        {
            return Some((ws.width / ws.columns, ws.height / ws.rows));
        }
        self.tmux_cell
    }

    /// Brings the screen to `places`: uploads pixels the terminal has not
    /// seen, moves the placements that moved, deletes the ones whose window
    /// went. The uploaded pixels stay when a placement goes — `Ctrl-^` is
    /// about to want them, and the terminal evicts its own store by quota.
    pub fn sync(&mut self, ed: &Editor, places: &[Place]) -> std::io::Result<()> {
        if self.support == Support::None {
            return Ok(());
        }
        let mut out = std::io::stdout().lock();
        let mut wrote = false;

        let wanted: HashSet<(u64, u32)> = places.iter().map(|p| (p.id, p.pid)).collect();
        let gone: Vec<(u64, u32)> =
            self.placed.keys().filter(|key| !wanted.contains(key)).copied().collect();
        for (id, pid) in gone {
            self.placed.remove(&(id, pid));
            self.write_seq(&mut out, &format!("\x1b_Ga=d,d=i,i={id},p={pid},q=2\x1b\\"))?;
            wrote = true;
        }

        // Under tmux the wrapped escapes land on the *outer* terminal, whose
        // coordinates are the pane's plus where tmux put the pane. Asked per
        // sync that writes, because panes move.
        let mut offset: Option<(u16, u16)> = None;

        for place in places {
            // The frame's "generation" is its size: a new tile size is a
            // new picture under the old id. A picture's is its edit
            // generation and the zoom its pixels were scaled by, so `:zoom`
            // re-uploads and a cursor move does not.
            let stamp = match place.id {
                FRAME_ID => {
                    let (w, h) = place.frame_size();
                    ((w as u64) << 32) | h as u64
                }
                id => match ed.image_with_id(id) {
                    Some(img) => (img.generation << 32) | place.scale as u64,
                    None => continue,
                },
            };
            if self.needs_send(place.id, stamp) {
                let (pixels, w, h): (std::borrow::Cow<[u8]>, u32, u32) = match place.id {
                    FRAME_ID => {
                        let (w, h) = place.frame_size();
                        (frame_pixels(w, h).into(), w, h)
                    }
                    id => {
                        let img = ed.image_with_id(id).expect("looked up above");
                        if place.zoom() == 1.0 {
                            ((&img.rgba).into(), img.width, img.height)
                        } else {
                            let (px, w, h) = scaled(&img.rgba, img.width, img.height, place.zoom());
                            (px.into(), w, h)
                        }
                    }
                };
                self.transmit(&mut out, place.id, &pixels, w, h)?;
                self.mark_sent(place.id, stamp);
                // Re-sending replaces the pixels and the terminal forgets
                // the placements with them: forget ours too, so they are
                // re-emitted below.
                self.placed.retain(|&(id, _), _| id != place.id);
                wrote = true;
            }
            if self.placed.get(&(place.id, place.pid)) == Some(place) {
                continue;
            }
            let (top, left) = *offset.get_or_insert_with(|| self.pane_offset());
            let (x, y, w, h) = place.crop;
            let (ox, oy) = place.offset;
            // Save the cursor, move to the cell, place without moving the
            // cursor (`C=1`), come back. `q=2` everywhere: nothing here reads
            // responses once the event thread owns stdin. `z=-1` keeps the
            // picture under text glyphs and above background fills, so what
            // the renderer draws over these cells stays readable; the tile
            // frame rides at `z=0`, over the picture.
            // `c=`/`r=` only on the terminal-scaled path: given, the
            // terminal stretches the crop to those cells; absent, it draws
            // the pixels at their size.
            let fit = match place.fit {
                true => format!(",c={},r={}", place.cols, place.rows),
                false => String::new(),
            };
            let seq = format!(
                "\x1b7\x1b[{};{}H\x1b_Ga=p,i={},p={},x={x},y={y},w={w},h={h},X={ox},Y={oy},z={}{fit},C=1,q=2\x1b\\\x1b8",
                place.row + top + 1,
                place.col + left + 1,
                place.id,
                place.pid,
                place.z,
            );
            self.write_seq(&mut out, &seq)?;
            self.placed.insert((place.id, place.pid), *place);
            wrote = true;
        }
        if wrote {
            out.flush()?;
        }
        Ok(())
    }

    /// Takes every image bi put up back down — placements and pixels both.
    ///
    /// Called on the way out, while the screen is still bi's: leaving the
    /// alternate screen does not delete placements, and an editor that quits
    /// leaving a photograph floating over the shell has not quit. Uppercase
    /// `I`, so the terminal's store is freed too — only what bi uploaded,
    /// never another program's images.
    pub fn clear(&mut self) -> std::io::Result<()> {
        if self.support == Support::None || self.sent.is_empty() {
            return Ok(());
        }
        let mut out = std::io::stdout().lock();
        let ids: Vec<u64> = self.sent.drain().map(|(id, _)| id).collect();
        for id in ids {
            self.write_seq(&mut out, &format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\"))?;
        }
        self.placed.clear();
        out.flush()
    }

    /// Where the pane sits on the outer terminal — zero outside tmux, where
    /// the pane *is* the terminal.
    fn pane_offset(&self) -> (u16, u16) {
        if self.support != Support::Tmux {
            return (0, 0);
        }
        tmux_out(&["display-message", "-p", "#{pane_top},#{pane_left}"])
            .and_then(|s| parse_pair(s.trim()))
            .unwrap_or((0, 0))
    }

    /// One escape sequence, as the terminal will receive it. Direct writes it
    /// as it is; tmux wraps it in the passthrough DCS, every `ESC` doubled,
    /// which is the format tmux unwraps on the far side.
    fn write_seq(&self, out: &mut impl Write, seq: &str) -> std::io::Result<()> {
        match self.support {
            Support::Tmux => {
                write!(out, "\x1bPtmux;{}\x1b\\", seq.replace('\x1b', "\x1b\x1b"))
            }
            _ => out.write_all(seq.as_bytes()),
        }
    }

    /// Whether `id`'s pixels at `generation` have yet to go up.
    fn needs_send(&self, id: u64, generation: u64) -> bool {
        self.sent.get(&id) != Some(&generation)
    }

    fn mark_sent(&mut self, id: u64, generation: u64) {
        self.sent.insert(id, generation);
    }

    /// Uploads one image's pixels, PNG on the wire.
    ///
    /// `f=100` rather than raw RGBA: a fraction of the bytes — this may be
    /// crossing an SSH connection — and the terminal decodes natively.
    /// Encoded from the core's RGBA once, here, because the core's job ended
    /// at pixels.
    fn transmit(
        &self,
        out: &mut impl Write,
        id: u64,
        rgba: &[u8],
        width: u32,
        height: u32,
    ) -> std::io::Result<()> {
        let mut png = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new(&mut png);
        image::ImageEncoder::write_image(
            encoder,
            rgba,
            width,
            height,
            image::ExtendedColorType::Rgba8,
        )
        .map_err(std::io::Error::other)?;

        let data = base64::engine::general_purpose::STANDARD.encode(&png);
        let mut chunks = data.as_bytes().chunks(4096).peekable();
        let mut first = true;
        while let Some(chunk) = chunks.next() {
            let more = if chunks.peek().is_some() { 1 } else { 0 };
            let head = match first {
                true => format!("\x1b_Ga=t,f=100,t=d,i={id},q=2,m={more};"),
                false => format!("\x1b_Gm={more};"),
            };
            first = false;
            let seq = format!("{head}{}\x1b\\", std::str::from_utf8(chunk).expect("base64"));
            self.write_seq(out, &seq)?;
        }
        Ok(())
    }
}

/// The most RGBA the crisp path will upload for one picture: 4096×4096.
/// Past it the original goes up and the terminal scales. See
/// `docs/specs/zoom.md`.
pub const CRISP_BUDGET: u64 = 4096 * 4096 * 4;

/// The size a `width`×`height` picture has at `zoom`, in display pixels —
/// never less than one on a side.
pub fn scaled_size(width: u32, height: u32, zoom: f32) -> (u32, u32) {
    (
        ((width as f64 * zoom as f64).round() as u32).max(1),
        ((height as f64 * zoom as f64).round() as u32).max(1),
    )
}

/// Whether a `width`×`height` picture at `zoom` fits the crisp budget.
pub fn crisp(width: u32, height: u32, zoom: f32) -> bool {
    let (w, h) = scaled_size(width, height, zoom);
    w as u64 * h as u64 * 4 <= CRISP_BUDGET
}

/// `rgba` at `zoom`, nearest-neighbour: each display pixel is the source
/// pixel under it, no blending. Pixel art stays pixel art; a photograph at
/// a tenth drops rows, which is what a thumbnail does. Returns the pixels
/// and their size.
pub fn scaled(rgba: &[u8], width: u32, height: u32, zoom: f32) -> (Vec<u8>, u32, u32) {
    let (w, h) = scaled_size(width, height, zoom);
    if (w, h) == (width, height) {
        return (rgba.to_vec(), w, h);
    }
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        let sy = ((y as f64 / zoom as f64) as u32).min(height - 1);
        let row = &rgba[(sy * width * 4) as usize..((sy + 1) * width * 4) as usize];
        for x in 0..w {
            let sx = ((x as f64 / zoom as f64) as u32).min(width - 1);
            out.extend_from_slice(&row[(sx * 4) as usize..(sx * 4 + 4) as usize]);
        }
    }
    (out, w, h)
}

/// The tilemap cursor: a `width`×`height` RGBA frame, transparent inside,
/// with a one-pixel border whose colour alternates white and black every
/// three pixels — a dash that reads on any tile, light or dark.
pub fn frame_pixels(width: u32, height: u32) -> Vec<u8> {
    let mut px = vec![0u8; (width * height * 4) as usize];
    let mut paint = |x: u32, y: u32, along: u32| {
        let v = if (along / 3) % 2 == 0 { 255 } else { 0 };
        let i = ((y * width + x) * 4) as usize;
        px[i..i + 4].copy_from_slice(&[v, v, v, 255]);
    };
    for x in 0..width {
        paint(x, 0, x);
        paint(x, height - 1, x);
    }
    for y in 0..height {
        paint(0, y, y);
        paint(width - 1, y, y);
    }
    px
}

/// Whether, and how, the terminal can draw pixels.
///
/// Outside tmux this is a handshake, not an environment guess — `$TERM` lies
/// in both directions over SSH, which is exactly where a Raspberry Pi gets
/// used. Sends a graphics query (`a=q` — answer, do not draw) followed by
/// primary device attributes. Every terminal answers DA1, so the read ends;
/// one that also answered `i=31;OK` speaks graphics. Must run after raw mode
/// is on and before the event-reader thread takes stdin.
///
/// Inside tmux the handshake cannot run — tmux answers DA1 itself and drops
/// the graphics reply — so tmux is asked instead: passthrough has to be
/// allowed, and the attached client's terminal has to be one that speaks the
/// protocol. An environment guess after all, but tmux's own live answer
/// about its client, not a stale variable.
pub fn detect() -> Support {
    if std::env::var_os("TMUX").is_some() {
        return detect_tmux();
    }
    match std::env::var("TERM") {
        Ok(term) if !term.is_empty() && term != "dumb" => {}
        _ => return Support::None,
    }
    use std::io::IsTerminal;
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    if !stdin.is_terminal() || !stdout.is_terminal() {
        return Support::None;
    }
    {
        let mut out = stdout.lock();
        if out.write_all(b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c").is_err()
            || out.flush().is_err()
        {
            return Support::None;
        }
    }

    let mut buf: Vec<u8> = Vec::new();
    let mut lock = stdin.lock();
    let mut byte = [0u8; 1];
    loop {
        match lock.read(&mut byte) {
            Ok(1) => buf.push(byte[0]),
            _ => break,
        }
        // The DA1 answer is `ESC [ ? … c`, and the graphics answer — when
        // there is one — arrived before it: seeing DA1 end is seeing
        // everything. Keys a fast typist got in first scroll past harmlessly;
        // none of them can spell `ESC [ ?`.
        if byte[0] == b'c'
            && let Some(at) = find(&buf, b"\x1b[?")
            && buf[at..].contains(&b'c')
        {
            break;
        }
        if buf.len() > 2048 {
            break;
        }
    }
    match find(&buf, b"_Gi=31;OK").is_some() {
        true => Support::Direct,
        false => Support::None,
    }
}

/// tmux's own answers about itself and its client.
///
/// `allow-passthrough` must be `on` or `all` — `on` is enough, and has the
/// nicety of dropping escapes from panes that are not visible. The option
/// does not exist before tmux 3.3, where the query fails and the answer is
/// honestly no.
fn detect_tmux() -> Support {
    let allow = tmux_out(&["show", "-Apv", "allow-passthrough"]);
    if !matches!(allow.as_deref().map(str::trim), Some("on" | "all")) {
        return Support::None;
    }
    let term = tmux_out(&["display-message", "-p", "#{client_termname}"]).unwrap_or_default();
    match ["kitty", "ghostty", "wezterm"].iter().any(|name| term.contains(name)) {
        true => Support::Tmux,
        false => Support::None,
    }
}

fn tmux_out(args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("tmux").args(args).output().ok()?;
    match output.status.success() {
        true => Some(String::from_utf8_lossy(&output.stdout).into_owned()),
        false => None,
    }
}

/// `"12,34"` → `(12, 34)`.
fn parse_pair(s: &str) -> Option<(u16, u16)> {
    let (a, b) = s.split_once(',')?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The frame is transparent inside, and its border alternates black and
    /// white in three-pixel runs so it reads on any tile.
    #[test]
    fn the_frame_is_a_dashed_ring_around_nothing() {
        let (w, h) = (8, 6);
        let px = frame_pixels(w, h);
        assert_eq!(px.len(), (w * h * 4) as usize);
        let at = |x: u32, y: u32| &px[((y * w + x) * 4) as usize..((y * w + x) * 4 + 4) as usize];
        assert_eq!(at(3, 2), &[0, 0, 0, 0], "inside is clear");
        assert_eq!(at(0, 0), &[255, 255, 255, 255]);
        assert_eq!(at(2, 0), &[255, 255, 255, 255]);
        assert_eq!(at(3, 0), &[0, 0, 0, 255], "the run switches after three");
        assert_eq!(at(6, 0)[3], 255, "every border pixel is opaque");
        assert_eq!(at(0, 5)[3], 255);
        assert_eq!(at(7, 3)[3], 255);
        assert_eq!(at(0, 3)[3], 255);
    }

    /// A placement of a stale generation goes back up under the same id and
    /// its placement is re-emitted; a frame of a new size the same way.
    #[test]
    fn stale_pixels_are_sent_again() {
        let mut g = Graphics::new(Support::None);
        assert!(g.needs_send(7, 0));
        g.mark_sent(7, 0);
        assert!(!g.needs_send(7, 0));
        assert!(g.needs_send(7, 1), "an edit moved the generation");
    }

    /// Nearest-neighbour: each source pixel becomes a zoom×zoom block going
    /// up, and every zoom-th pixel survives going down. No blending.
    #[test]
    fn scaling_is_nearest_neighbour_both_ways() {
        let a = [1, 1, 1, 255];
        let b = [2, 2, 2, 255];
        let c = [3, 3, 3, 255];
        let d = [4, 4, 4, 255];
        let src: Vec<u8> = [a, b, c, d].concat();

        let (up, w, h) = scaled(&src, 2, 2, 2.0);
        assert_eq!((w, h), (4, 4));
        let rows: Vec<Vec<u8>> = up.chunks(16).map(|r| r.to_vec()).collect();
        assert_eq!(rows[0], [a, a, b, b].concat());
        assert_eq!(rows[1], [a, a, b, b].concat());
        assert_eq!(rows[2], [c, c, d, d].concat());
        assert_eq!(rows[3], [c, c, d, d].concat());

        let (down, w, h) = scaled(&src, 2, 2, 0.5);
        assert_eq!((w, h), (1, 1));
        assert_eq!(down, a, "the top-left survives");

        let (same, w, h) = scaled(&src, 2, 2, 1.0);
        assert_eq!((w, h), (2, 2));
        assert_eq!(same, src);
    }

    #[test]
    fn the_budget_decides_crisp_or_terminal_scaled() {
        assert!(crisp(100, 100, 2.0));
        assert!(crisp(4096, 4096, 1.0), "exactly the budget");
        assert!(!crisp(4096, 4096, 2.0));
        assert!(!crisp(200, 200, 32.0));
    }
}
