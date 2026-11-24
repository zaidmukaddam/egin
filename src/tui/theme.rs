//! egin's terminal palette: the site's neutral grays, and lime, egin's one accent (the N's diagonal in the logo).

use ratatui::style::Color;
use std::sync::OnceLock;

pub const LIME: u32 = 0xc6f432;

pub struct Theme {
    pub accent: Color,
    pub bg: Color,
    pub band: Color,
    pub code: Color,
    pub code_fg: Color,
    pub inline: Color,
    pub hover: Color,
    pub fg: Color,
    pub text: Color,
    pub muted: Color,
    pub dim: Color,
    pub faint: Color,
    pub border: Color,
    pub border_on: Color,
    pub err: Color,
    pub warn: Color,
    pub add_fg: Color,
    pub add_bg: Color,
    pub del_fg: Color,
    pub del_bg: Color,
}

pub fn th() -> &'static Theme {
    static T: OnceLock<Theme> = OnceLock::new();
    T.get_or_init(|| Theme {
        accent: rgb(LIME),
        bg: rgb(0x101010),
        band: rgb(0x1c1c1c),
        code: rgb(0x0a0a0a),
        code_fg: rgb(0xc8c8c8),
        inline: rgb(0x262626),
        hover: rgb(0x2a2a2a),
        fg: rgb(0xf2f2f2),
        text: rgb(0xd4d4d4),
        muted: rgb(0x9a9a9a),
        dim: rgb(0x6e6e6e),
        faint: rgb(0x3a3a3a),
        border: rgb(0x2e2e2e),
        border_on: rgb(0x505050),
        err: rgb(0xec7a82),
        warn: rgb(0xdcb36c),
        add_fg: rgb(0xa6e3a6),
        add_bg: rgb(0x10291a),
        del_fg: rgb(0xf3a7ad),
        del_bg: rgb(0x301418),
    })
}

/// The color `t` (0..=1) of the way from `a` to `b`, for the animated rails.
pub fn mix(a: u32, b: u32, t: f64) -> Color {
    let t = t.clamp(0.0, 1.0);
    let ch = |s: u32| {
        let (x, y) = ((a >> s) & 0xff, (b >> s) & 0xff);
        ((x as f64 + (y as f64 - x as f64) * t).round() as u32) << s
    };
    rgb(ch(16) | ch(8) | ch(0))
}

fn truecolor() -> bool {
    static T: OnceLock<bool> = OnceLock::new();
    *T.get_or_init(|| {
        let ct = std::env::var("COLORTERM").unwrap_or_default();
        let prog = std::env::var("TERM_PROGRAM").unwrap_or_default();
        ct.contains("truecolor") || ct.contains("24bit") || (!prog.is_empty() && prog != "Apple_Terminal")
    })
}

/// A color as 24-bit RGB, or the nearest xterm-256 slot where the terminal has no truecolor (Terminal.app).
pub fn rgb(hex: u32) -> Color {
    let (r, g, b) = ((hex >> 16) as u8, (hex >> 8) as u8, hex as u8);
    if truecolor() {
        return Color::Rgb(r, g, b);
    }
    if r == g && g == b {
        // The 24-step gray ramp, 8..238, then the cube's ends.
        return Color::Indexed(match r {
            0..=3 => 16,
            248..=255 => 231,
            v => 232 + ((v as u16 - 3) * 23 / 245) as u8,
        });
    }
    let c = |v: u8| ((v as u16 * 5 + 127) / 255) as u8;
    Color::Indexed(16 + 36 * c(r) + 6 * c(g) + c(b))
}
