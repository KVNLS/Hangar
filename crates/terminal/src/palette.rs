use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{self, Color, NamedColor};

/// A resolved 24-bit colour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    pub const fn from_hex(hex: u32) -> Self {
        Self::new((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
    }

    pub const fn to_hex(self) -> u32 {
        ((self.r as u32) << 16) | ((self.g as u32) << 8) | self.b as u32
    }

    /// Scales every channel by `factor` (0.0..=1.0), used for SGR 2 (dim/faint).
    pub fn scaled(self, factor: f32) -> Self {
        let scale = |c: u8| (f32::from(c) * factor).round().clamp(0.0, 255.0) as u8;
        Self::new(scale(self.r), scale(self.g), scale(self.b))
    }
}

impl From<ansi::Rgb> for Rgb {
    fn from(c: ansi::Rgb) -> Self {
        Self::new(c.r, c.g, c.b)
    }
}

impl From<Rgb> for ansi::Rgb {
    fn from(c: Rgb) -> Self {
        ansi::Rgb {
            r: c.r,
            g: c.g,
            b: c.b,
        }
    }
}

const DIM_FACTOR: f32 = 0.66;

/// Default colours of the terminal. Programs can override individual entries at runtime
/// (OSC 4/10/11/12); those overrides live in the emulator and take precedence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Palette {
    /// The 16 ANSI colours: 0-7 normal, 8-15 bright.
    pub ansi: [Rgb; 16],
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            ansi: [
                Rgb::from_hex(0x2b2d33),
                Rgb::from_hex(0xe0645a),
                Rgb::from_hex(0x8fc76a),
                Rgb::from_hex(0xe5c07b),
                Rgb::from_hex(0x61a0e8),
                Rgb::from_hex(0xc47fd5),
                Rgb::from_hex(0x56b6c2),
                Rgb::from_hex(0xc8ccd4),
                Rgb::from_hex(0x5c6370),
                Rgb::from_hex(0xff7a70),
                Rgb::from_hex(0xa8e08a),
                Rgb::from_hex(0xffd88a),
                Rgb::from_hex(0x7ab8ff),
                Rgb::from_hex(0xdc9cf0),
                Rgb::from_hex(0x74d0dc),
                Rgb::from_hex(0xf0f2f5),
            ],
            foreground: Rgb::from_hex(0xd6d9de),
            background: Rgb::from_hex(0x16181c),
            cursor: Rgb::from_hex(0xe8e8e8),
        }
    }
}

impl Palette {
    /// Resolves a 256-colour index: 0-15 ANSI, 16-231 the 6x6x6 cube, 232-255 the grey ramp.
    pub fn indexed(&self, index: u8) -> Rgb {
        match index {
            0..=15 => self.ansi[usize::from(index)],
            16..=231 => {
                let i = index - 16;
                let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
                Rgb::new(level(i / 36), level((i / 6) % 6), level(i % 6))
            }
            232..=255 => {
                let v = 8 + (index - 232) * 10;
                Rgb::new(v, v, v)
            }
        }
    }

    fn named(&self, color: NamedColor) -> Rgb {
        let index = color as usize;
        if index < 16 {
            return self.ansi[index];
        }
        match color {
            NamedColor::Foreground => self.foreground,
            NamedColor::Background => self.background,
            NamedColor::Cursor => self.cursor,
            NamedColor::BrightForeground => self.ansi[15],
            NamedColor::DimForeground => self.foreground.scaled(DIM_FACTOR),
            dim => self.named(dim.to_bright()).scaled(DIM_FACTOR),
        }
    }

    /// Resolves an emulator colour, honouring runtime overrides set by the program.
    pub(crate) fn resolve(&self, color: Color, overrides: &Colors) -> Rgb {
        match color {
            Color::Spec(rgb) => rgb.into(),
            Color::Indexed(i) => overrides[usize::from(i)]
                .map(Rgb::from)
                .unwrap_or_else(|| self.indexed(i)),
            Color::Named(named) => overrides[named]
                .map(Rgb::from)
                .unwrap_or_else(|| self.named(named)),
        }
    }

    /// Resolves the colour at a raw index of alacritty's colour table (used for OSC queries).
    pub(crate) fn resolve_index(&self, index: usize, overrides: &Colors) -> Option<Rgb> {
        if index >= alacritty_terminal::term::color::COUNT {
            return None;
        }
        if let Some(rgb) = overrides[index] {
            return Some(rgb.into());
        }
        match u8::try_from(index) {
            Ok(i) => Some(self.indexed(i)),
            Err(_) => named_from_index(index).map(|n| self.named(n)),
        }
    }

    /// Resolves the foreground of a cell drawn with SGR 2 (dim).
    pub(crate) fn resolve_dim(&self, color: Color, overrides: &Colors) -> Rgb {
        match color {
            Color::Named(named) if (named as usize) < 8 || named == NamedColor::Foreground => {
                let dim = named.to_dim();
                overrides[dim]
                    .map(Rgb::from)
                    .unwrap_or_else(|| self.named(dim))
            }
            other => self.resolve(other, overrides).scaled(DIM_FACTOR),
        }
    }
}

fn named_from_index(index: usize) -> Option<NamedColor> {
    use NamedColor::*;
    let named = match index {
        256 => Foreground,
        257 => Background,
        258 => Cursor,
        259 => DimBlack,
        260 => DimRed,
        261 => DimGreen,
        262 => DimYellow,
        263 => DimBlue,
        264 => DimMagenta,
        265 => DimCyan,
        266 => DimWhite,
        267 => BrightForeground,
        268 => DimForeground,
        _ => return None,
    };
    Some(named)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ansi_indices_map_to_palette() {
        let p = Palette::default();
        for i in 0..16u8 {
            assert_eq!(p.indexed(i), p.ansi[usize::from(i)]);
        }
    }

    #[test]
    fn colour_cube_matches_xterm() {
        let p = Palette::default();
        assert_eq!(p.indexed(16), Rgb::new(0, 0, 0));
        assert_eq!(p.indexed(196), Rgb::new(255, 0, 0));
        assert_eq!(p.indexed(46), Rgb::new(0, 255, 0));
        assert_eq!(p.indexed(21), Rgb::new(0, 0, 255));
        assert_eq!(p.indexed(231), Rgb::new(255, 255, 255));
        assert_eq!(p.indexed(67), Rgb::new(95, 135, 175));
    }

    #[test]
    fn grey_ramp_matches_xterm() {
        let p = Palette::default();
        assert_eq!(p.indexed(232), Rgb::new(8, 8, 8));
        assert_eq!(p.indexed(244), Rgb::new(128, 128, 128));
        assert_eq!(p.indexed(255), Rgb::new(238, 238, 238));
    }

    #[test]
    fn truecolor_passes_through_and_overrides_win() {
        let p = Palette::default();
        let mut overrides = Colors::default();
        let spec = Color::Spec(ansi::Rgb { r: 1, g: 2, b: 3 });
        assert_eq!(p.resolve(spec, &overrides), Rgb::new(1, 2, 3));

        assert_eq!(p.resolve(Color::Indexed(1), &overrides), p.ansi[1]);
        overrides[1] = Some(ansi::Rgb { r: 9, g: 9, b: 9 });
        assert_eq!(p.resolve(Color::Indexed(1), &overrides), Rgb::new(9, 9, 9));
        assert_eq!(
            p.resolve(Color::Named(NamedColor::Red), &overrides),
            Rgb::new(9, 9, 9)
        );
    }

    #[test]
    fn special_colours_and_dim() {
        let p = Palette::default();
        let o = Colors::default();
        assert_eq!(
            p.resolve(Color::Named(NamedColor::Foreground), &o),
            p.foreground
        );
        assert_eq!(
            p.resolve(Color::Named(NamedColor::Background), &o),
            p.background
        );
        assert_eq!(
            p.resolve_dim(Color::Named(NamedColor::Red), &o),
            p.ansi[1].scaled(DIM_FACTOR)
        );
        assert_eq!(
            p.resolve_index(NamedColor::Background as usize, &o),
            Some(p.background)
        );
        assert_eq!(p.resolve_index(9999, &o), None);
    }

    #[test]
    fn hex_round_trip() {
        assert_eq!(Rgb::from_hex(0x123456).to_hex(), 0x123456);
    }
}
