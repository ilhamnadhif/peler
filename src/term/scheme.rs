//! Terminal colour schemes and the xterm 256-colour palette.

use eframe::egui::Color32;

#[derive(Clone, Debug, PartialEq)]
pub struct Scheme {
    pub name: &'static str,
    pub dark: bool,
    pub foreground: Color32,
    pub background: Color32,
    pub cursor: Color32,
    pub selection: Color32,
    /// ANSI 0–15: black, red, green, yellow, blue, magenta, cyan, white, then bright variants.
    pub ansi: [Color32; 16],
}

const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

const fn scheme(
    name: &'static str,
    dark: bool,
    fg: u32,
    bg: u32,
    cursor: u32,
    selection: u32,
    ansi: [u32; 16],
) -> Scheme {
    let mut colors = [Color32::BLACK; 16];
    let mut i = 0;
    while i < 16 {
        colors[i] = hex(ansi[i]);
        i += 1;
    }
    Scheme {
        name,
        dark,
        foreground: hex(fg),
        background: hex(bg),
        cursor: hex(cursor),
        selection: hex(selection),
        ansi: colors,
    }
}

pub const SCHEMES: &[Scheme] = &[
    scheme(
        "Peler Dark",
        true,
        0xd4d7dd,
        0x17191e,
        0xa094f8,
        0x3a3566,
        [
            0x1e2127, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xabb2bf,
            0x5c6370, 0xef7b84, 0xa9d48a, 0xf0d08c, 0x74bdf7, 0xd38aea, 0x67c7d3, 0xe6e8ec,
        ],
    ),
    scheme(
        "Peler Light",
        false,
        0x383a42,
        0xfbfbfc,
        0x6d5ae6,
        0xdcd6fb,
        [
            0x383a42, 0xe45649, 0x50a14f, 0xc18401, 0x4078f2, 0xa626a4, 0x0184bc, 0xa0a1a7,
            0x696c77, 0xf0705f, 0x5fb85e, 0xd49b1e, 0x5a8ef5, 0xbb3fb9, 0x1a9dd1, 0xfafafa,
        ],
    ),
    scheme(
        "Solarized Dark",
        true,
        0x839496,
        0x002b36,
        0x93a1a1,
        0x073642,
        [
            0x073642, 0xdc322f, 0x859900, 0xb58900, 0x268bd2, 0xd33682, 0x2aa198, 0xeee8d5,
            0x002b36, 0xcb4b16, 0x586e75, 0x657b83, 0x839496, 0x6c71c4, 0x93a1a1, 0xfdf6e3,
        ],
    ),
    scheme(
        "Solarized Light",
        false,
        0x657b83,
        0xfdf6e3,
        0x586e75,
        0xeee8d5,
        [
            0x073642, 0xdc322f, 0x859900, 0xb58900, 0x268bd2, 0xd33682, 0x2aa198, 0xeee8d5,
            0x002b36, 0xcb4b16, 0x586e75, 0x657b83, 0x839496, 0x6c71c4, 0x93a1a1, 0xfdf6e3,
        ],
    ),
    scheme(
        "Dracula",
        true,
        0xf8f8f2,
        0x282a36,
        0xf8f8f2,
        0x44475a,
        [
            0x21222c, 0xff5555, 0x50fa7b, 0xf1fa8c, 0xbd93f9, 0xff79c6, 0x8be9fd, 0xf8f8f2,
            0x6272a4, 0xff6e6e, 0x69ff94, 0xffffa5, 0xd6acff, 0xff92df, 0xa4ffff, 0xffffff,
        ],
    ),
    scheme(
        "Nord",
        true,
        0xd8dee9,
        0x2e3440,
        0xd8dee9,
        0x434c5e,
        [
            0x3b4252, 0xbf616a, 0xa3be8c, 0xebcb8b, 0x81a1c1, 0xb48ead, 0x88c0d0, 0xe5e9f0,
            0x4c566a, 0xbf616a, 0xa3be8c, 0xebcb8b, 0x81a1c1, 0xb48ead, 0x8fbcbb, 0xeceff4,
        ],
    ),
    scheme(
        "Gruvbox Dark",
        true,
        0xebdbb2,
        0x282828,
        0xebdbb2,
        0x504945,
        [
            0x282828, 0xcc241d, 0x98971a, 0xd79921, 0x458588, 0xb16286, 0x689d6a, 0xa89984,
            0x928374, 0xfb4934, 0xb8bb26, 0xfabd2f, 0x83a598, 0xd3869b, 0x8ec07c, 0xebdbb2,
        ],
    ),
    scheme(
        "Tomorrow Night",
        true,
        0xc5c8c6,
        0x1d1f21,
        0xc5c8c6,
        0x373b41,
        [
            0x1d1f21, 0xcc6666, 0xb5bd68, 0xf0c674, 0x81a2be, 0xb294bb, 0x8abeb7, 0xc5c8c6,
            0x969896, 0xd54e53, 0xb9ca4a, 0xe7c547, 0x7aa6da, 0xc397d8, 0x70c0b1, 0xeaeaea,
        ],
    ),
    scheme(
        "GitHub Light",
        false,
        0x24292f,
        0xffffff,
        0x0969da,
        0xcce5ff,
        [
            0x24292f, 0xcf222e, 0x116329, 0x4d2d00, 0x0969da, 0x8250df, 0x1b7c83, 0x6e7781,
            0x57606a, 0xa40e26, 0x1a7f37, 0x633c01, 0x218bff, 0xa475f9, 0x3192aa, 0x8c959f,
        ],
    ),
];

/// Looks up a scheme by name, falling back to the built-in dark or light default.
pub fn find(name: &str, dark: bool) -> &'static Scheme {
    SCHEMES
        .iter()
        .find(|s| s.name == name)
        .unwrap_or(if dark { &SCHEMES[0] } else { &SCHEMES[1] })
}

/// Colour for an xterm palette index 16–255 (6×6×6 cube, then a 24-step grey ramp).
pub fn xterm_256(index: u8) -> Color32 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    match index {
        16..=231 => {
            let i = index - 16;
            Color32::from_rgb(
                LEVELS[(i / 36) as usize],
                LEVELS[((i / 6) % 6) as usize],
                LEVELS[(i % 6) as usize],
            )
        }
        232..=255 => {
            let v = 8 + 10 * (index - 232);
            Color32::from_rgb(v, v, v)
        }
        _ => Color32::BLACK,
    }
}

impl Scheme {
    /// Palette colour 0–255 with this scheme's 16 ANSI colours.
    pub fn indexed(&self, index: u8) -> Color32 {
        if index < 16 {
            self.ansi[index as usize]
        } else {
            xterm_256(index)
        }
    }
}

/// Halves the distance to the background: used for SGR 2 (dim / faint).
pub fn dim(c: Color32, bg: Color32) -> Color32 {
    let mix = |a: u8, b: u8| ((a as u16 * 2 + b as u16) / 3) as u8;
    Color32::from_rgb(mix(c.r(), bg.r()), mix(c.g(), bg.g()), mix(c.b(), bg.b()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette() {
        assert_eq!(xterm_256(16), Color32::from_rgb(0, 0, 0));
        assert_eq!(xterm_256(231), Color32::from_rgb(255, 255, 255));
        assert_eq!(xterm_256(196), Color32::from_rgb(255, 0, 0));
        assert_eq!(xterm_256(232), Color32::from_rgb(8, 8, 8));
        assert_eq!(xterm_256(255), Color32::from_rgb(238, 238, 238));
        assert_eq!(find("nope", false).name, "Peler Light");
        assert!(SCHEMES.iter().filter(|s| s.dark).count() >= 3);
    }
}
