//! Color tokens ported from the asp/desktop "Vault Editor" design `styles.css`.
//! Light is the design's `:root`, dark is `[data-theme="dark"]`. Stored as
//! `Rgba` (gpui `rgb()`), which converts into the `Hsla`/`Fill` the element
//! style methods expect.

use gpui::{rgb, rgba, Rgba};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    pub dark: bool,
    pub bg: Rgba,
    pub bg_sub: Rgba,
    pub bg_input: Rgba,
    pub text: Rgba,
    pub text2: Rgba,
    pub text3: Rgba,
    pub faint: Rgba,
    pub faint2: Rgba,
    pub line: Rgba,
    pub overlay: Rgba,
    pub accent: Rgba,
}

impl Theme {
    pub fn light() -> Self {
        Theme {
            dark: false,
            bg: rgb(0xffffff),
            bg_sub: rgb(0xfafaf8),
            bg_input: rgb(0xfaf9f7),
            text: rgb(0x1c1917),
            text2: rgb(0x57534e),
            text3: rgb(0x78716c),
            faint: rgb(0xa8a29e),
            faint2: rgb(0xb0aaa2),
            line: rgb(0xededea),
            overlay: rgba(0x1c1917_4d),
            accent: rgb(0x3d63dd),
        }
    }

    pub fn dark() -> Self {
        Theme {
            dark: true,
            bg: rgb(0x1b1b1e),
            bg_sub: rgb(0x161618),
            bg_input: rgb(0x232327),
            text: rgb(0xececec),
            text2: rgb(0xb6b2ab),
            text3: rgb(0x9b968d),
            faint: rgb(0x827d74),
            faint2: rgb(0x6f6a62),
            line: rgb(0x2d2d31),
            overlay: rgba(0x0000009e),
            accent: rgb(0x3d63dd),
        }
    }

    /// Accent at ~13% opacity (the design's `{accent}22` soft fill).
    pub fn accent_soft(&self) -> Rgba {
        let a = self.accent;
        Rgba { a: 0.13, ..a }
    }
}
