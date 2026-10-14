//! Design tokens (design system v0.1, `design/Hangar.dc.html`). One palette per appearance; views
//! read them through [`ActiveTokens::tokens`], never hard-coded colors.

use std::sync::LazyLock;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{App, Hsla, rgba};

macro_rules! palette {
    ($($(#[$doc:meta])* $name:ident),* $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq)]
        pub struct Palette<T> {
            $($(#[$doc])* pub $name: T,)*
        }

        impl<T: Copy> Palette<T> {
            pub fn map<U>(&self, f: impl Fn(T) -> U) -> Palette<U> {
                Palette { $($name: f(self.$name),)* }
            }

            pub fn entries(&self) -> Vec<(&'static str, T)> {
                vec![$((stringify!($name), self.$name),)*]
            }
        }
    };
}

palette!(
    /// Window, pane content.
    app,
    /// Sidebar, tab bars, right panel.
    panel,
    /// Cards, sheets, palette.
    elev,
    /// Row hover, chips.
    hover,
    /// Selected row.
    active,
    /// Terminal, logs, code.
    term,
    /// Dividers, card borders.
    line,
    /// Inputs, buttons.
    line2,
    /// Body, titles.
    tx,
    /// Labels, inactive tabs.
    tx2,
    /// Metadata, hints. Never interactive labels (below 4.5:1).
    tx3,
    /// Primary action, focus, working.
    acc,
    /// Hunk headers, badges.
    accbg,
    /// Text on accent.
    accfg,
    /// Waiting for approval.
    amb,
    ambbg,
    /// Needs input.
    vio,
    viobg,
    /// Ready for review, additions.
    grn,
    grnbg,
    /// Failed, deletions.
    red,
    redbg,
    /// Added diff line.
    addbg,
    /// Removed diff line.
    delbg,
    /// Modal scrim, the only translucent token.
    scrim,
);

pub type Tokens = Palette<Hsla>;

/// `0xRRGGBBAA` values.
pub const DARK_HEX: Palette<u32> = Palette {
    app: 0x0D0E10FF,
    panel: 0x131417FF,
    elev: 0x1A1B1FFF,
    hover: 0x1D1F23FF,
    active: 0x25272CFF,
    term: 0x0F1013FF,
    line: 0x23252AFF,
    line2: 0x33363CFF,
    tx: 0xE4E5E8FF,
    tx2: 0x9DA1A9FF,
    tx3: 0x6A6E76FF,
    acc: 0x7B93FFFF,
    accbg: 0x1B2140FF,
    accfg: 0x0D0E10FF,
    amb: 0xE3A53DFF,
    ambbg: 0x2A2113FF,
    vio: 0xB48CF2FF,
    viobg: 0x241D34FF,
    grn: 0x4FB583FF,
    grnbg: 0x13281EFF,
    red: 0xE5584FFF,
    redbg: 0x2D1716FF,
    addbg: 0x12261AFF,
    delbg: 0x2C1517FF,
    scrim: 0x00000080,
};

pub const LIGHT_HEX: Palette<u32> = Palette {
    app: 0xF4F4F5FF,
    panel: 0xFFFFFFFF,
    elev: 0xFFFFFFFF,
    hover: 0xEEEFF1FF,
    active: 0xE6E7EAFF,
    term: 0xFAFAFBFF,
    line: 0xE4E5E8FF,
    line2: 0xD1D3D8FF,
    tx: 0x17181BFF,
    tx2: 0x575B63FF,
    tx3: 0x878B93FF,
    acc: 0x3F5BE0FF,
    accbg: 0xE9EDFDFF,
    accfg: 0xFFFFFFFF,
    amb: 0xA8650CFF,
    ambbg: 0xFBF0DCFF,
    vio: 0x7647C9FF,
    viobg: 0xF1EAFCFF,
    grn: 0x1F8049FF,
    grnbg: 0xE3F3E9FF,
    red: 0xC9362DFF,
    redbg: 0xFCE8E6FF,
    addbg: 0xE4F4E8FF,
    delbg: 0xFBE7E6FF,
    scrim: 0x18191E47,
};

static DARK: LazyLock<Tokens> = LazyLock::new(|| DARK_HEX.map(|hex| rgba(hex).into()));
static LIGHT: LazyLock<Tokens> = LazyLock::new(|| LIGHT_HEX.map(|hex| rgba(hex).into()));

pub fn tokens_for(dark: bool) -> &'static Tokens {
    if dark { &DARK } else { &LIGHT }
}

pub trait ActiveTokens {
    /// The palette matching the current theme mode.
    fn tokens(&self) -> &'static Tokens;
}

impl ActiveTokens for App {
    fn tokens(&self) -> &'static Tokens {
        tokens_for(self.theme().is_dark())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luminance(hex: u32) -> f64 {
        let channel = |shift: u32| {
            let c = f64::from((hex >> shift) & 0xFF) / 255.0;
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(24) + 0.7152 * channel(16) + 0.0722 * channel(8)
    }

    fn contrast(a: u32, b: u32) -> f64 {
        let (la, lb) = (luminance(a), luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    #[test]
    fn text_pairs_meet_aa_except_tertiary() {
        for p in [DARK_HEX, LIGHT_HEX] {
            for bg in [p.app, p.panel, p.elev, p.term] {
                for fg in [p.tx, p.tx2] {
                    assert!(contrast(fg, bg) >= 4.5, "{fg:08x} on {bg:08x}");
                }
            }
            assert!(contrast(p.accfg, p.acc) >= 4.5, "on-accent");
        }
    }

    #[test]
    fn only_scrim_is_translucent() {
        for p in [DARK_HEX, LIGHT_HEX] {
            for (name, hex) in p.entries() {
                assert_eq!(hex & 0xFF == 0xFF, name != "scrim", "{name}");
            }
        }
    }
}
