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
}

impl Place {
    /// Whether any of this placement's cells fall inside `x, y, w, h`.
    pub fn intersects(&self, x: u16, y: u16, w: u16, h: u16) -> bool {
        self.col < x + w && x < self.col + self.cols && self.row < y + h && y < self.row + self.rows
    }
}

/// What the terminal is currently showing, and what it already holds.
pub struct Graphics {
    support: Support,
    /// Ids whose pixels the terminal has been sent. Pixels go up once; every
    /// frame after moves a placement, which is metadata.
    sent: HashSet<u64>,
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
        Self { support, sent: HashSet::new(), placed: HashMap::new(), tmux_cell }
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
            if !self.sent.contains(&place.id) {
                let Some(img) = ed.image_with_id(place.id) else { continue };
                self.transmit(&mut out, img)?;
                self.sent.insert(place.id);
                wrote = true;
            }
            if self.placed.get(&(place.id, place.pid)) == Some(place) {
                continue;
            }
            let (top, left) = *offset.get_or_insert_with(|| self.pane_offset());
            let (x, y, w, h) = place.crop;
            // Save the cursor, move to the cell, place without moving the
            // cursor (`C=1`), come back. `q=2` everywhere: nothing here reads
            // responses once the event thread owns stdin. `z=-1` keeps the
            // picture under text glyphs and above background fills, so what
            // the renderer draws over these cells stays readable.
            let seq = format!(
                "\x1b7\x1b[{};{}H\x1b_Ga=p,i={},p={},x={x},y={y},w={w},h={h},z=-1,C=1,q=2\x1b\\\x1b8",
                place.row + top + 1,
                place.col + left + 1,
                place.id,
                place.pid,
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
        let ids: Vec<u64> = self.sent.drain().collect();
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

    /// Uploads one image's pixels, PNG on the wire.
    ///
    /// `f=100` rather than raw RGBA: a fraction of the bytes — this may be
    /// crossing an SSH connection — and the terminal decodes natively.
    /// Encoded from the core's RGBA once, here, because the core's job ended
    /// at pixels.
    fn transmit(&self, out: &mut impl Write, img: &bi::img::Img) -> std::io::Result<()> {
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
            let head = match first {
                true => format!("\x1b_Ga=t,f=100,t=d,i={},q=2,m={more};", img.id),
                false => format!("\x1b_Gm={more};"),
            };
            first = false;
            let seq = format!("{head}{}\x1b\\", std::str::from_utf8(chunk).expect("base64"));
            self.write_seq(out, &seq)?;
        }
        Ok(())
    }
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
