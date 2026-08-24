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

/// One image at one spot, cropped — what `render` wants on screen this
/// frame. Cells for the destination, pixels for the crop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Place {
    /// The image's core id — `bi::img::Img::id`.
    pub id: u64,
    /// Destination, in cells, screen-absolute.
    pub col: u16,
    pub row: u16,
    /// x, y, width, height of the source rectangle, in pixels.
    pub crop: (u32, u32, u32, u32),
}

/// What the terminal is currently showing, and what it already holds.
pub struct Graphics {
    supported: bool,
    /// Ids whose pixels the terminal has been sent. Pixels go up once; every
    /// frame after moves a placement, which is metadata.
    sent: HashSet<u64>,
    /// The placements on screen, by image id. Re-creating a placement under
    /// the same placement id replaces it atomically, so only what moved is
    /// ever rewritten — which is what keeps scrolling flicker-free.
    placed: HashMap<u64, Place>,
}

impl Graphics {
    pub fn new(supported: bool) -> Self {
        Self { supported, sent: HashSet::new(), placed: HashMap::new() }
    }

    /// One cell's size in pixels, when the terminal both draws images and
    /// says how big its cells are. Asked per frame — a font change
    /// mid-session is legal and TIOCGWINSZ is one cheap ioctl. `None` means
    /// the placeholder path: no arithmetic on a guess.
    pub fn cell_size(&self) -> Option<(u16, u16)> {
        if !self.supported {
            return None;
        }
        let ws = ratatui::crossterm::terminal::window_size().ok()?;
        if ws.columns == 0 || ws.rows == 0 || ws.width == 0 || ws.height == 0 {
            return None;
        }
        Some((ws.width / ws.columns, ws.height / ws.rows))
    }

    /// Brings the screen to `places`: uploads pixels the terminal has not
    /// seen, moves the placements that moved, deletes the ones whose window
    /// went. The uploaded pixels stay when a placement goes — `Ctrl-^` is
    /// about to want them, and the terminal evicts its own store by quota.
    pub fn sync(&mut self, ed: &Editor, places: &[Place]) -> std::io::Result<()> {
        if !self.supported {
            return Ok(());
        }
        let mut out = std::io::stdout().lock();
        let mut wrote = false;

        let wanted: HashSet<u64> = places.iter().map(|p| p.id).collect();
        let gone: Vec<u64> =
            self.placed.keys().filter(|id| !wanted.contains(id)).copied().collect();
        for id in gone {
            self.placed.remove(&id);
            write!(out, "\x1b_Ga=d,d=i,i={id},p=1,q=2\x1b\\")?;
            wrote = true;
        }

        for place in places {
            if !self.sent.contains(&place.id) {
                let Some(img) = ed.image_with_id(place.id) else { continue };
                transmit(&mut out, img)?;
                self.sent.insert(place.id);
                wrote = true;
            }
            if self.placed.get(&place.id) == Some(place) {
                continue;
            }
            let (x, y, w, h) = place.crop;
            // Save the cursor, move to the cell, place without moving the
            // cursor (`C=1`), come back. `q=2` everywhere: nothing here reads
            // responses once the event thread owns stdin.
            write!(
                out,
                "\x1b7\x1b[{};{}H\x1b_Ga=p,i={},p=1,x={x},y={y},w={w},h={h},C=1,q=2\x1b\\\x1b8",
                place.row + 1,
                place.col + 1,
                place.id,
            )?;
            self.placed.insert(place.id, *place);
            wrote = true;
        }
        if wrote {
            out.flush()?;
        }
        Ok(())
    }
}

/// Uploads one image's pixels, PNG on the wire.
///
/// `f=100` rather than raw RGBA: a fraction of the bytes — this may be
/// crossing an SSH connection — and the terminal decodes natively. Encoded
/// from the core's RGBA once, here, because the core's job ended at pixels.
fn transmit(out: &mut impl Write, img: &bi::img::Img) -> std::io::Result<()> {
    let mut png = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut png);
    image::ImageEncoder::write_image(
        encoder,
        &img.rgba,
        img.width,
        img.height,
        image::ExtendedColorType::Rgba8,
    )
    .map_err(std::io::Error::other)?;

    let data = base64::engine::general_purpose::STANDARD.encode(&png);
    let mut chunks = data.as_bytes().chunks(4096).peekable();
    let mut first = true;
    while let Some(chunk) = chunks.next() {
        let more = if chunks.peek().is_some() { 1 } else { 0 };
        match first {
            true => write!(out, "\x1b_Ga=t,f=100,t=d,i={},q=2,m={more};", img.id)?,
            false => write!(out, "\x1b_Gm={more};")?,
        }
        first = false;
        out.write_all(chunk)?;
        write!(out, "\x1b\\")?;
    }
    Ok(())
}

/// Whether the terminal speaks the protocol. A handshake, not an environment
/// guess — `$TERM` lies in both directions over SSH, which is exactly where
/// a Raspberry Pi gets used.
///
/// Sends a graphics query (`a=q` — answer, do not draw) followed by primary
/// device attributes. Every terminal answers DA1, so the read ends; one that
/// also answered `i=31;OK` speaks graphics. Must run after raw mode is on
/// and before the event-reader thread takes stdin.
pub fn detect() -> bool {
    use std::io::IsTerminal;
    if std::env::var_os("TMUX").is_some() {
        return false;
    }
    match std::env::var("TERM") {
        Ok(term) if !term.is_empty() && term != "dumb" => {}
        _ => return false,
    }
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    if !stdin.is_terminal() || !stdout.is_terminal() {
        return false;
    }
    {
        let mut out = stdout.lock();
        if out.write_all(b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c").is_err()
            || out.flush().is_err()
        {
            return false;
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
    find(&buf, b"_Gi=31;OK").is_some()
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}
