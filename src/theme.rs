//! Colours for the UI chrome and the 16-colour terminal palette.

use gpui::{rgb, rgba, Hsla, Rgba};

pub fn hex(c: u32) -> Hsla {
    rgb(c).into()
}

pub fn hexa(c: u32, a: f32) -> Hsla {
    let mut h: Hsla = rgb(c).into();
    h.a = a;
    h
}

/// Base colour of the window; drawn with the user's opacity on top of the blur.
pub const BG: u32 = 0x0e1116;
pub const FG: u32 = 0xd6dde6;
pub const MUTED: u32 = 0x7d8794;
pub const SUBTLE: u32 = 0x4a525e;
pub const ACCENT: u32 = 0x7aa2f7;
pub const DANGER: u32 = 0xf7768e;
pub const OK: u32 = 0x9ece6a;
pub const CURSOR: u32 = 0xc0caf5;
pub const SELECTION: u32 = 0x3d59a1;

pub fn panel() -> Hsla {
    hexa(0x171b22, 0.96)
}
pub fn border() -> Hsla {
    rgba(0xffffff14).into()
}
pub fn hover() -> Hsla {
    rgba(0xffffff0f).into()
}
pub fn active_tab() -> Hsla {
    rgba(0xffffff17).into()
}

/// ANSI 0–15 (normal then bright). Soft, low-glare palette.
pub const ANSI: [u32; 16] = [
    0x1d2129, // black
    0xf7768e, // red
    0x9ece6a, // green
    0xe0af68, // yellow
    0x7aa2f7, // blue
    0xbb9af7, // magenta
    0x7dcfff, // cyan
    0xc0caf5, // white
    0x4a525e, // bright black
    0xff8fa3, // bright red
    0xb5e08a, // bright green
    0xf0c58a, // bright yellow
    0x99b8ff, // bright blue
    0xcfb6ff, // bright magenta
    0xa4e0ff, // bright cyan
    0xf2f4f8, // bright white
];

/// xterm 256-colour value for an index.
pub fn indexed(i: u8) -> (u8, u8, u8) {
    match i {
        0..=15 => {
            let c = ANSI[i as usize];
            ((c >> 16) as u8, (c >> 8) as u8, c as u8)
        }
        16..=231 => {
            let i = i - 16;
            let steps = [0u8, 95, 135, 175, 215, 255];
            (steps[(i / 36) as usize], steps[((i / 6) % 6) as usize], steps[(i % 6) as usize])
        }
        232..=255 => {
            let v = 8 + (i - 232) * 10;
            (v, v, v)
        }
    }
}

pub fn rgb_to_hsla(r: u8, g: u8, b: u8) -> Hsla {
    Rgba { r: r as f32 / 255.0, g: g as f32 / 255.0, b: b as f32 / 255.0, a: 1.0 }.into()
}
