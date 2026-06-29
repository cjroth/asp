#![allow(dead_code)] // intentional theme API (FONT_SANS, selection) kept for wiring
//! Design tokens ported from desktop `src/styles.css` (see docs/DESIGN_SPEC.md §1).
//! A `Theme` carries the resolved palette + accent; light/dark mirror the CSS vars.

use gpui::{rgb, rgba, Hsla, Rgba};
use serde::{Deserialize, Serialize};

/// Font family names. Mono/serif are bundled (see assets); sans falls back to the
/// platform UI font via the cosmic text system's "sans-serif" alias.
pub const FONT_SANS: &str = "sans-serif";
pub const FONT_SERIF: &str = "Newsreader";
pub const FONT_MONO: &str = "JetBrains Mono";

/// The 8 vault accent hues (HSL hue degrees) from the design.
pub const VAULT_HUES: [f32; 8] = [222.0, 158.0, 32.0, 268.0, 344.0, 188.0, 46.0, 12.0];

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Appearance {
    Light,
    Dark,
}

/// Resolved palette. Colors are stored as `Rgba` (gpui converts to `Hsla` on use).
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub appearance: Appearance,
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
    /// Accent (default hue 222 → #3d63dd). Per-vault accents derive from `VAULT_HUES`.
    pub accent: Rgba,
    // History-track semantic colors.
    pub create: Rgba,
    pub edit: Rgba,
    pub rename: Rgba,
    pub delete: Rgba,
}

impl Theme {
    pub fn light() -> Self {
        Theme {
            appearance: Appearance::Light,
            bg: rgb(0xffffff),
            bg_sub: rgb(0xfafaf8),
            bg_input: rgb(0xfaf9f7),
            text: rgb(0x1c1917),
            text2: rgb(0x57534e),
            text3: rgb(0x78716c),
            faint: rgb(0xa8a29e),
            faint2: rgb(0xb0aaa2),
            line: rgb(0xededea),
            overlay: rgba(0x1c19174d), // rgba(28,25,23,0.30)
            accent: rgb(0x3d63dd),
            create: rgb(0x3fa45a),
            edit: rgb(0x3d63dd),
            rename: rgb(0xd9a93d),
            delete: rgb(0xd96a6a),
        }
    }

    pub fn dark() -> Self {
        Theme {
            appearance: Appearance::Dark,
            bg: rgb(0x1b1b1e),
            bg_sub: rgb(0x161618),
            bg_input: rgb(0x232327),
            text: rgb(0xececec),
            text2: rgb(0xb6b2ab),
            text3: rgb(0x9b968d),
            faint: rgb(0x827d74),
            faint2: rgb(0x6f6a62),
            line: rgb(0x2d2d31),
            overlay: rgba(0x0000009e), // rgba(0,0,0,0.62)
            accent: rgb(0x3d63dd),
            create: rgb(0x3fa45a),
            edit: rgb(0x3d63dd),
            rename: rgb(0xd9a93d),
            delete: rgb(0xd96a6a),
        }
    }

    /// `::selection` background — accent at 16% opacity.
    pub fn selection(&self) -> Hsla {
        let mut h: Hsla = self.accent.into();
        h.a = 0.16;
        h
    }

    /// Accent at the given alpha (the design's `accent + '22'` ≈ 0.13 soft fills).
    pub fn accent_alpha(&self, a: f32) -> Hsla {
        let mut h: Hsla = self.accent.into();
        h.a = a;
        h
    }
}

/// A pastel avatar background for a vault hue: `hsl(hue 44% 94%)` (light) per spec.
pub fn vault_avatar_bg(hue: f32) -> Hsla {
    gpui::hsla(hue / 360.0, 0.44, 0.94, 1.0)
}

/// The avatar border for a vault hue: `hsl(hue 36% 86%)`.
pub fn vault_avatar_border(hue: f32) -> Hsla {
    gpui::hsla(hue / 360.0, 0.36, 0.86, 1.0)
}

/// The monogram text color for a vault hue: `hsl(hue 42% 40%)`.
pub fn vault_monogram(hue: f32) -> Hsla {
    gpui::hsla(hue / 360.0, 0.42, 0.40, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palettes_have_expected_anchors() {
        assert_eq!(Theme::light().bg, rgb(0xffffff));
        assert_eq!(Theme::light().accent, rgb(0x3d63dd));
        assert_eq!(Theme::dark().bg, rgb(0x1b1b1e));
    }

    #[test]
    fn selection_is_accent_at_16pct() {
        let s = Theme::light().selection();
        assert!((s.a - 0.16).abs() < 1e-6);
    }

    #[test]
    fn vault_hues_count() {
        assert_eq!(VAULT_HUES.len(), 8);
    }
}
