//! Connect screen — data-driven over `AspApp` (see DESIGN_SPEC.md §3).

use gpui::{div, prelude::*, px, Context, Div, FontWeight, MouseButton, MouseDownEvent, SharedString};

use crate::app::{AspApp, ConnectRow};
use crate::icons::icon;
use crate::theme::{self, Appearance};

pub fn render(app: &AspApp, cx: &mut Context<AspApp>) -> Div {
    let t = app.theme;

    let logo = div()
        .size(px(26.0))
        .rounded(px(7.0))
        .bg(t.accent)
        .flex()
        .items_center()
        .justify_center()
        .child(div().size(px(9.0)).rounded_full().bg(gpui::white()));

    let platform = div()
        .flex()
        .items_center()
        .gap(px(6.0))
        .text_size(px(12.0))
        .text_color(t.faint)
        .child(div().size(px(8.0)).rounded(px(2.0)).bg(t.accent))
        .child(if app.is_web {
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
                .font_family(theme::FONT_MONO)
                .text_size(px(16.0))
                .font_weight(FontWeight(600.0))
                .text_color(t.text)
                .child("asp"),
        )
        .child(div().flex_1())
        .child(platform)
        .child(
            div()
                .id("theme-toggle")
                .size(px(28.0))
                .rounded(px(8.0))
                .border_1()
                .border_color(t.line)
                .bg(t.bg)
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|s| s.bg(t.line))
                .on_click(cx.listener(|this, _ev, _window, cx| {
                    this.toggle_theme();
                    cx.notify();
                }))
                .child(icon(
                    if t.appearance == Appearance::Dark { "theme-sun" } else { "theme-moon" },
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

    let new_btn = div()
        .id("new-vault")
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
        .cursor_pointer()
        .child(icon("plus", px(16.0), t.bg))
        .child("New Vault");

    let connect_btn = div()
        .id("connect-vault")
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
        .cursor_pointer()
        .child(icon("connect", px(15.0), t.text2))
        .child("Connect Vault");

    let actions = div().flex().gap(px(10.0)).child(new_btn).child(connect_btn);

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
    for (i, row) in app.connect_rows.iter().enumerate() {
        card = card.child(vault_row(app, row, i, i > 0, cx));
    }

    let list = if app.connect_rows.is_empty() {
        div()
    } else {
        div().mt(px(26.0)).mb(px(9.0)).child(list_label).child(card)
    };

    let footer = div()
        .mt(px(28.0))
        .text_size(px(11.5))
        .text_color(t.faint2)
        .flex()
        .items_center()
        .gap(px(6.0))
        .child(icon("user", px(12.0), t.faint2))
        .child(format!("This device · {}", app.fingerprint));

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

fn vault_row(
    app: &AspApp,
    row: &ConnectRow,
    idx: usize,
    border_top: bool,
    cx: &mut Context<AspApp>,
) -> impl IntoElement {
    let t = app.theme;
    let glyph: SharedString = match &row.emoji {
        Some(e) => e.clone().into(),
        None => row
            .name
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_default()
            .into(),
    };

    let avatar = div()
        .size(px(34.0))
        .rounded(px(10.0))
        .bg(theme::vault_avatar_bg(row.hue))
        .border_1()
        .border_color(theme::vault_avatar_border(row.hue))
        .flex()
        .items_center()
        .justify_center()
        .when(row.emoji.is_some(), |d| d.text_size(px(18.0)))
        .when(row.emoji.is_none(), |d| {
            d.text_size(px(13.6))
                .font_weight(FontWeight(600.0))
                .text_color(theme::vault_monogram(row.hue))
        })
        .child(glyph);

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
                .child(row.name.clone()),
        )
        .child(
            div()
                .mt(px(3.0))
                .flex()
                .items_center()
                .gap(px(6.0))
                .text_size(px(11.0))
                .text_color(t.faint)
                .child(icon(if row.is_web { "globe" } else { "folder" }, px(12.0), t.faint))
                .child(row.location.clone()),
        );

    let trailing = div()
        .text_size(px(11.5))
        .text_color(t.faint)
        .when(row.loading, |d| d.opacity(0.55))
        .child(row.time.clone());

    let id = row.id.clone();
    let menu_id = row.id.clone();
    let menu_name = row.name.clone();
    div()
        .id(SharedString::from(format!("vault-{idx}")))
        .flex()
        .items_center()
        .gap(px(13.0))
        .px(px(15.0))
        .py(px(13.0))
        .when(border_top, |d| d.border_t_1().border_color(t.line))
        .when(row.loading, |d| d.opacity(0.55))
        .when(!row.loading, |d| {
            d.cursor_pointer()
                .hover(|s| s.bg(t.bg_sub))
                .on_click(cx.listener(move |this, _ev, _window, cx| { this.open_vault(&id); cx.notify(); }))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, ev: &MouseDownEvent, _window, cx| {
                        this.open_vault_menu(
                            &menu_id,
                            &menu_name,
                            f32::from(ev.position.x),
                            f32::from(ev.position.y),
                        );
                        cx.notify();
                    }),
                )
        })
        .child(avatar)
        .child(content)
        .child(trailing)
        .when(!row.loading, |d| {
            d.child(icon("chevron-right", px(15.0), gpui::rgb(0xcfc9c1)))
        })
}
