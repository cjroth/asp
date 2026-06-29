//! User preferences (accent, writing column, theme, sidebar/history sizes,
//! hidden files, pretty names), ported from desktop `src/vault/prefs.ts`. The
//! localStorage/DOM bits are the app layer's job; here we keep the data model,
//! defaults, and the size clamps (pure + tested).

use crate::theme::Appearance;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrontmatterStyle {
    Card,
    Banner,
    Below,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Prefs {
    /// Accent color as 0xRRGGBB (default 0x3d63dd).
    pub accent: u32,
    pub frontmatter_style: FrontmatterStyle,
    pub writing_column: bool,
    pub theme: Appearance,
    pub sidebar_w: f32,
    pub hist_bar_h: f32,
    pub show_hidden: bool,
    pub pretty_names: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            accent: 0x3d63dd,
            frontmatter_style: FrontmatterStyle::Below,
            writing_column: true,
            theme: Appearance::Light,
            sidebar_w: 266.0,
            hist_bar_h: 150.0,
            show_hidden: false,
            pretty_names: false,
        }
    }
}

pub const SIDEBAR_MIN: f32 = 200.0;
pub const SIDEBAR_MAX: f32 = 460.0;

pub fn clamp_sidebar(w: f32) -> f32 {
    w.max(SIDEBAR_MIN).min(SIDEBAR_MAX)
}

pub const HISTBAR_MIN: f32 = 96.0;
pub const HISTBAR_MAX: f32 = 640.0;
pub const HISTBAR_COLLAPSE: f32 = 72.0;

pub fn clamp_hist_bar(h: f32) -> f32 {
    h.max(HISTBAR_MIN).min(HISTBAR_MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_design() {
        let p = Prefs::default();
        assert_eq!(p.accent, 0x3d63dd);
        assert_eq!(p.sidebar_w, 266.0);
        assert_eq!(p.hist_bar_h, 150.0);
        assert!(p.writing_column);
        assert!(!p.show_hidden);
        assert_eq!(p.theme, Appearance::Light);
    }

    #[test]
    fn clamp_sidebar_bounds() {
        assert_eq!(clamp_sidebar(10.0), SIDEBAR_MIN);
        assert_eq!(clamp_sidebar(9999.0), SIDEBAR_MAX);
        assert_eq!(clamp_sidebar(300.0), 300.0);
    }

    #[test]
    fn clamp_hist_bar_bounds() {
        assert_eq!(clamp_hist_bar(10.0), HISTBAR_MIN);
        assert_eq!(clamp_hist_bar(9999.0), HISTBAR_MAX);
        assert_eq!(clamp_hist_bar(150.0), 150.0);
    }
}
