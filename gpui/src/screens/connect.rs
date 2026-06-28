//! Connect screen — your vaults, "New Vault" / "Connect Vault", device footer.
//! Ported from desktop `src/App.tsx` Connect screen (see DESIGN_SPEC.md §3).

use gpui::{
    div, prelude::*, px, FontWeight, SharedString, Window,
};

use crate::icons::icon;
use crate::theme::{self, Appearance, Theme, FONT_MONO};

/// One vault row in the list card (fixture-or-engine-driven).
#[derive(Clone)]
pub struct VaultCard {
    pub name: SharedString,
    pub hue: f32,
    /// emoji glyph, or None → monogram from the first letter of `name`.
    pub emoji: Option<SharedString>,
    pub location: SharedString,
    pub time: SharedString,
    pub loading: bool,
    pub is_web: bool,
}

impl VaultCard {
    fn glyph(&self) -> SharedString {
        if let Some(e) = &self.emoji {
            return e.clone();
        }
        self.name
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_default()
            .into()
    }
}

/// The Connect screen view.
pub struct ConnectScreen {
    pub theme: Theme,
    pub vaults: Vec<VaultCard>,
    pub fingerprint: SharedString,
    pub is_web: bool,
}

impl ConnectScreen {
    /// A representative fixture for visual checks / screenshots.
    pub fn fixture(theme: Theme) -> Self {
        ConnectScreen {
            theme,
            vaults: vec![
                VaultCard {
                    name: "Research Notes".into(),
                    hue: theme::VAULT_HUES[0],
                    emoji: None,
                    location: "~/vaults/research".into(),
                    time: "2h ago".into(),
                    loading: false,
                    is_web: false,
                },
                VaultCard {
                    name: "Journal".into(),
                    hue: theme::VAULT_HUES[3],
                    emoji: Some("📔".into()),
                    location: "~/Documents/journal".into(),
                    time: "yesterday".into(),
                    loading: false,
                    is_web: false,
                },
                VaultCard {
                    name: "Shared Wiki".into(),
                    hue: theme::VAULT_HUES[1],
                    emoji: None,
                    location: "Opening…".into(),
                    time: "Opening…".into(),
                    loading: true,
                    is_web: false,
                },
            ],
            fingerprint: "a1b2c3d4".into(),
            is_web: false,
        }
    }
}

impl Render for ConnectScreen {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;

        // App logo: 26×26 accent rounded square with a 9×9 white circle inside.
        let logo = div()
            .size(px(26.0))
            .rounded(px(7.0))
            .bg(t.accent)
            .flex()
            .items_center()
            .justify_center()
            .child(div().size(px(9.0)).rounded_full().bg(gpui::white()));

        // Platform indicator (right of header).
        let platform = div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .text_size(px(12.0))
            .text_color(t.faint)
            .child(div().size(px(8.0)).rounded(px(2.0)).bg(t.accent))
            .child(if self.is_web {
                "Saved in this browser"
            } else {
                "On this computer"
            });

        let header = div()
            .flex()
            .items_center()
            .gap(px(11.0))
            .mb(px(34.0))
            .child(logo)
            .child(
                div()
                    .font_family(FONT_MONO)
                    .text_size(px(16.0))
                    .font_weight(FontWeight(600.0))
                    .text_color(t.text)
                    .child("asp"),
            )
            .child(div().flex_1())
            .child(platform)
            .child(
                // Theme toggle button (28×28 bordered): moon in light, sun in dark.
                div()
                    .size(px(28.0))
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(t.line)
                    .bg(t.bg)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon(
                        if t.appearance == Appearance::Dark {
                            "theme-sun"
                        } else {
                            "theme-moon"
                        },
                        px(16.0),
                        t.text3,
                    )),
            );

        let headline = div()
            .text_size(px(25.0))
            .font_weight(FontWeight(600.0))
            .text_color(t.text)
            .mb(px(22.0))
            .child("Your vaults");

        // Action buttons row.
        let new_btn = div()
            .flex_1()
            .h(px(46.0))
            .rounded(px(11.0))
            .bg(t.text)
            .text_color(t.bg)
            .flex()
            .items_center()
            .justify_center()
            .gap(px(8.0))
            .text_size(px(14.0))
            .font_weight(FontWeight(500.0))
            .child(icon("plus", px(16.0), t.bg))
            .child("New Vault");

        let connect_btn = div()
            .flex_1()
            .h(px(46.0))
            .rounded(px(11.0))
            .border_1()
            .border_color(t.line)
            .bg(t.bg)
            .text_color(t.text2)
            .flex()
            .items_center()
            .justify_center()
            .gap(px(8.0))
            .text_size(px(14.0))
            .font_weight(FontWeight(500.0))
            .child(icon("connect", px(15.0), t.text2))
            .child("Connect Vault");

        let actions = div().flex().gap(px(10.0)).child(new_btn).child(connect_btn);

        // Vault list card.
        let list_label = div()
            .text_size(px(11.0))
            .font_weight(FontWeight(600.0))
            .text_color(t.faint2)
            .pl(px(3.0))
            .mb(px(8.0))
            .child("SAVED VAULTS");

        let mut card = div()
            .border_1()
            .border_color(t.line)
            .rounded(px(14.0))
            .overflow_hidden()
            .bg(t.bg);
        for (i, v) in self.vaults.iter().enumerate() {
            card = card.child(self.vault_row(v, i > 0));
        }

        let list = div()
            .mt(px(26.0))
            .mb(px(9.0))
            .child(list_label)
            .child(card);

        let footer = div()
            .mt(px(28.0))
            .text_size(px(11.5))
            .text_color(t.faint2)
            .flex()
            .items_center()
            .gap(px(6.0))
            .child(icon("user", px(12.0), t.faint2))
            .child(format!("This device · {}", self.fingerprint));

        // Centered card column on the bg-sub backdrop.
        div()
            .size_full()
            .bg(t.bg_sub)
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .p(px(32.0))
            .child(
                div()
                    .w(px(452.0))
                    .flex()
                    .flex_col()
                    .child(header)
                    .child(headline)
                    .child(actions)
                    .child(list)
                    .child(footer),
            )
    }
}

impl ConnectScreen {
    fn vault_row(&self, v: &VaultCard, border_top: bool) -> impl IntoElement {
        let t = self.theme;
        let avatar = div()
            .size(px(34.0))
            .rounded(px(10.0))
            .bg(theme::vault_avatar_bg(v.hue))
            .border_1()
            .border_color(theme::vault_avatar_border(v.hue))
            .flex()
            .items_center()
            .justify_center()
            .when(v.emoji.is_some(), |d| d.text_size(px(18.0)))
            .when(v.emoji.is_none(), |d| {
                d.text_size(px(13.6))
                    .font_weight(FontWeight(600.0))
                    .text_color(theme::vault_monogram(v.hue))
            })
            .child(v.glyph());

        let content = div()
            .flex_1()
            .min_w(px(0.0))
            .flex()
            .flex_col()
            .child(
                div()
                    .text_size(px(14.5))
                    .font_weight(FontWeight(500.0))
                    .text_color(t.text)
                    .child(v.name.clone()),
            )
            .child(
                div()
                    .mt(px(3.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .text_size(px(11.0))
                    .text_color(t.faint)
                    .child(icon(
                        if v.is_web { "globe" } else { "folder" },
                        px(12.0),
                        t.faint,
                    ))
                    .child(v.location.clone()),
            );

        let trailing = div()
            .text_size(px(11.5))
            .text_color(t.faint)
            .when(v.loading, |d| d.opacity(0.55))
            .child(v.time.clone());

        div()
            .flex()
            .items_center()
            .gap(px(13.0))
            .px(px(15.0))
            .py(px(13.0))
            .when(border_top, |d| d.border_t_1().border_color(t.line))
            .when(v.loading, |d| d.opacity(0.55))
            .child(avatar)
            .child(content)
            .child(trailing)
            .when(!v.loading, |d| {
                d.child(icon("chevron-right", px(15.0), gpui::rgb(0xcfc9c1)))
            })
    }
}
