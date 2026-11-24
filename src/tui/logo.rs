//! The egin wordmark as a real image: the brand paths (Paper's "egin brand assets", the same as
//! site/src/components/Logo.astro) rasterized at the terminal's own cell size and shown with the kitty graphics
//! protocol (kitty, Ghostty and terminals built on it, WezTerm) or iTerm2's inline images. Character cells can only
//! draw its slants as staircases, so terminals with neither get a typographic mark instead.

use base64::Engine;
use std::{
    io::Write,
    time::{Duration, Instant},
};

const BARS: &[&[(f64, f64)]] = &[
    &[(0., 0.), (90., 0.), (76.2, 20.), (0., 20.)],
    &[(0., 50.), (76., 50.), (62.2, 70.), (0., 70.)],
    &[(0., 100.), (76.2, 100.), (90., 120.), (0., 120.)],
    &[(110., 0.), (210., 0.), (196.2, 20.), (110., 20.)],
    &[(110., 50.), (130., 50.), (130., 100.), (190., 100.), (190., 80.), (160., 80.), (160., 60.), (210., 60.), (210., 120.), (110., 120.)],
    &[(230., 0.), (250., 0.), (250., 120.), (230., 120.)],
    &[(270., 0.), (290., 0.), (290., 120.), (270., 120.)],
    &[(350., 0.), (370., 0.), (370., 120.), (350., 120.)],
];
const SLASH: &[(f64, f64)] = &[(270., 0.), (292., 0.), (370., 120.), (348., 120.)];
const FG: [u8; 3] = [0xec, 0xec, 0xec];
const LIME: [u8; 3] = [0xc6, 0xf4, 0x32];
const ID: u32 = 7187;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Proto {
    Kitty,
    Iterm,
}

pub struct Logo {
    proto: Proto,
    pub cols: u16,
    pub rows: u16,
    png: Vec<u8>,
    rgba: Vec<u8>,
    px: (u32, u32),
    sent: bool,
    shown: Option<(u16, u16)>,
}

fn inside(p: &[(f64, f64)], x: f64, y: f64) -> bool {
    let mut c = false;
    for i in 0..p.len() {
        let ((x1, y1), (x2, y2)) = (p[i], p[(i + p.len() - 1) % p.len()]);
        if (y1 > y) != (y2 > y) && x < (x2 - x1) * (y - y1) / (y2 - y1) + x1 {
            c = !c;
        }
    }
    c
}

/// The mark fitted to `h` pixels tall at the left of a `w` × `h` box, as straight-alpha RGBA, 4×4 supersampled.
pub fn rgba(w: u32, h: u32) -> Vec<u8> {
    let scale = (h as f64 / 120.0).min(w as f64 / 370.0);
    let top = (h as f64 - 120.0 * scale) / 2.0;
    let mut out = vec![0u8; (w * h * 4) as usize];
    const S: u32 = 4;
    for py in 0..h {
        for px in 0..w {
            let (mut lime, mut bar) = (0u32, 0u32);
            for sy in 0..S {
                for sx in 0..S {
                    let x = (px as f64 + (sx as f64 + 0.5) / S as f64) / scale;
                    let y = (py as f64 + (sy as f64 + 0.5) / S as f64 - top) / scale;
                    if inside(SLASH, x, y) {
                        lime += 1;
                    } else if BARS.iter().any(|b| inside(b, x, y)) {
                        bar += 1;
                    }
                }
            }
            let n = lime + bar;
            if n == 0 {
                continue;
            }
            let i = ((py * w + px) * 4) as usize;
            for c in 0..3 {
                out[i + c] = ((LIME[c] as u32 * lime + FG[c] as u32 * bar) / n) as u8;
            }
            out[i + 3] = (255 * n / (S * S)) as u8;
        }
    }
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = !0u32;
    for b in data {
        c ^= *b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
}

/// A PNG of RGBA pixels, with stored (uncompressed) deflate blocks: a few kilobytes, sent once.
pub fn png(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity(((w * 4 + 1) * h) as usize);
    for row in rgba.chunks((w * 4) as usize) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut z = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65_535).collect();
    for (i, b) in blocks.iter().enumerate() {
        z.push(u8::from(i + 1 == blocks.len()));
        z.extend_from_slice(&(b.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(b.len() as u16)).to_le_bytes());
        z.extend_from_slice(b);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for x in &raw {
        a = (a + *x as u32) % 65_521;
        b = (b + a) % 65_521;
    }
    z.extend_from_slice(&(b << 16 | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |kind: &[u8], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    };
    let mut ihdr = w.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    out
}

/// Ask the terminal whether it speaks the kitty graphics protocol. Call in raw mode, before anything else
/// reads stdin: the reply (or its absence before the device-attributes answer) is read here.
fn kitty_supported() -> bool {
    let mut out = std::io::stdout();
    if write!(out, "\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c").and_then(|_| out.flush()).is_err() {
        return false;
    }
    let deadline = Instant::now() + Duration::from_millis(400);
    let mut buf = vec![];
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let mut pfd = libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 };
        // SAFETY: one valid pollfd for the process's stdin.
        if unsafe { libc::poll(&mut pfd, 1, left.as_millis() as i32) } <= 0 {
            break;
        }
        let mut b = [0u8; 512];
        // SAFETY: reads into a local buffer of the length given.
        let n = unsafe { libc::read(0, b.as_mut_ptr().cast(), b.len()) };
        if n <= 0 {
            break;
        }
        buf.extend_from_slice(&b[..n as usize]);
        // The device-attributes answer (ESC [ ? … c) comes after any graphics reply.
        if let Some(i) = buf.windows(3).position(|w| w == b"\x1b[?") {
            if buf[i..].contains(&b'c') {
                break;
            }
        }
    }
    String::from_utf8_lossy(&buf).contains("_Gi=31;OK")
}

pub fn detect() -> Option<Proto> {
    if std::env::var("EGIN_NO_IMAGES").is_ok_and(|v| !v.is_empty()) {
        return None;
    }
    let prog = std::env::var("TERM_PROGRAM").unwrap_or_default();
    // tmux and screen pass neither protocol through by default.
    if std::env::var("TMUX").is_ok() || std::env::var("TERM").unwrap_or_default().starts_with("screen") {
        return None;
    }
    if kitty_supported() {
        return Some(Proto::Kitty);
    }
    matches!(prog.as_str(), "iTerm.app" | "WezTerm").then_some(Proto::Iterm)
}

impl Logo {
    /// The mark `rows` cells tall, as wide as its proportions need at the terminal's cell size.
    pub fn new(proto: Proto, rows: u16) -> Logo {
        let cell = crossterm::terminal::window_size()
            .ok()
            .filter(|s| s.width > 0 && s.height > 0 && s.columns > 0 && s.rows > 0)
            .map(|s| ((s.width / s.columns) as u32, (s.height / s.rows) as u32))
            .unwrap_or((10, 20));
        let cols = ((rows as u32 * cell.1) as f64 * 370.0 / 120.0 / cell.0 as f64).ceil() as u16;
        let px = (cols as u32 * cell.0, rows as u32 * cell.1);
        let rgba = rgba(px.0, px.1);
        let png = if proto == Proto::Iterm { png(px.0, px.1, &rgba) } else { vec![] };
        Logo { proto, cols, rows, png, rgba, px, sent: false, shown: None }
    }

    /// Show the mark with its top-left cell at `at` (0-based column, row), or hide it. Returns true when the
    /// screen must be repainted before calling again (iTerm2 images live in the cells they cover).
    pub fn show(&mut self, at: Option<(u16, u16)>) -> bool {
        if at == self.shown {
            return false;
        }
        let mut out = std::io::stdout();
        let b64 = base64::engine::general_purpose::STANDARD;
        match self.proto {
            Proto::Kitty => {
                if !self.sent {
                    let data = b64.encode(&self.rgba);
                    let chunks: Vec<&[u8]> = data.as_bytes().chunks(4096).collect();
                    for (i, c) in chunks.iter().enumerate() {
                        let more = u8::from(i + 1 < chunks.len());
                        let head = if i == 0 { format!("a=t,f=32,s={},v={},i={ID},q=2,m={more}", self.px.0, self.px.1) } else { format!("m={more}") };
                        let _ = write!(out, "\x1b_G{head};{}\x1b\\", std::str::from_utf8(c).unwrap_or(""));
                    }
                    self.sent = true;
                }
                match at {
                    Some((x, y)) => {
                        let _ = write!(out, "\x1b7\x1b[{};{}H\x1b_Ga=p,i={ID},p=1,c={},r={},C=1,q=2\x1b\\\x1b8", y + 1, x + 1, self.cols, self.rows);
                    }
                    None => {
                        let _ = write!(out, "\x1b_Ga=d,d=i,i={ID},q=2\x1b\\");
                    }
                }
                self.shown = at;
                let _ = out.flush();
                false
            }
            Proto::Iterm => {
                // Moving or hiding it means painting over the cells it was drawn in first.
                if self.shown.is_some() {
                    self.shown = None;
                    return true;
                }
                if let Some((x, y)) = at {
                    let _ = write!(
                        out,
                        "\x1b7\x1b[{};{}H\x1b]1337;File=inline=1;size={};width={};height={};preserveAspectRatio=0:{}\x07\x1b8",
                        y + 1,
                        x + 1,
                        self.png.len(),
                        self.cols,
                        self.rows,
                        b64.encode(&self.png)
                    );
                    let _ = out.flush();
                }
                self.shown = at;
                false
            }
        }
    }

    /// Free the image when the UI closes.
    pub fn clear(&self) {
        if self.proto == Proto::Kitty && self.sent {
            let mut out = std::io::stdout();
            let _ = write!(out, "\x1b_Ga=d,d=I,i={ID},q=2\x1b\\");
            let _ = out.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_is_well_formed() {
        let (w, h) = (74, 24);
        let p = png(w, h, &rgba(w, h));
        assert_eq!(&p[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&p[12..16], b"IHDR");
        assert_eq!(u32::from_be_bytes(p[16..20].try_into().unwrap()), w);
        // The IHDR chunk's CRC covers its type and data.
        assert_eq!(u32::from_be_bytes(p[29..33].try_into().unwrap()), crc32(&p[12..29]));
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn raster_puts_lime_on_the_n_and_bars_on_the_e() {
        let (w, h) = (370, 120);
        let px = rgba(w, h);
        let at = |x: u32, y: u32| &px[((y * w + x) * 4) as usize..((y * w + x) * 4 + 4) as usize];
        assert_eq!(at(10, 10), [0xec, 0xec, 0xec, 255]);
        assert_eq!(at(10, 35)[3], 0, "the gap between the E's bars is clear");
        assert_eq!(at(320, 60), [0xc6, 0xf4, 0x32, 255]);
    }
}
