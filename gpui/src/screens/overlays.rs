//! Floating overlays — context menus (anchored) + modal dialogs (centered).
//! See DESIGN_SPEC.md §5 (menus/modals).

use gpui::{
    anchored, deferred, div, point, prelude::*, px, Context, Div, FontWeight,
};

use crate::app::{AspApp, Menu, Modal};
use crate::icons::icon;

/// The vault context menu (right-click a vault row on the Connect screen).
pub fn vault_menu(app: &AspApp, cx: &mut Context<AspApp>) -> Option<Div> {
    let Menu::Vault { id, name, x, y } = &app.menu else {
        return None;
    };
    let t = app.theme;
    let (id, name) = (id.clone(), name.clone());

    let item = |label: &str, icon_name: &'static str, danger: bool| {
        div()
            .flex()
            .items_center()
            .gap(px(10.0))
            .px(px(9.0))
            .py(px(8.0))
            .rounded(px(8.0))
            .text_size(px(13.0))
            .text_color(if danger { t.text2 } else { t.text })
            .hover(|s| s.bg(t.bg_sub))
            .cursor_pointer()
            .child(icon(icon_name, px(15.0), if danger { t.text2 } else { t.faint }))
            .child(label.to_string())
    };

    let menu = div()
        .w(px(200.0))
        .bg(t.bg)
        .border_1()
        .border_color(t.line)
        .rounded(px(12.0))
        .shadow_lg()
        .p(px(6.0))
        .flex()
        .flex_col()
        .gap(px(2.0))
        .on_mouse_down_out(cx.listener(|this, _ev, _window, cx| {
            this.close_menu();
            cx.notify();
        }))
        .child(item("Customize this vault…", "pencil", false).id("menu-customize").on_click(
            cx.listener(|this, _ev, _window, cx| {
                this.close_menu();
                cx.notify();
            }),
        ))
        .child(item("Share this vault…", "share", false).id("menu-share").on_click(cx.listener(
            |this, _ev, _window, cx| {
                this.close_menu();
                cx.notify();
            },
        )))
        .child(div().h(px(1.0)).my(px(4.0)).mx(px(6.0)).bg(t.line))
        .child({
            let (id, name) = (id.clone(), name.clone());
            item("Remove this vault…", "trash", true).id("menu-remove").on_click(cx.listener(
                move |this, _ev, _window, cx| {
                    this.open_remove(&id, &name);
                    cx.notify();
                },
            ))
        });

    Some(
        div().child(
            deferred(anchored().position(point(px(*x), px(*y))).child(menu)).priority(2),
        ),
    )
}

/// The "remove vault" confirmation modal.
pub fn remove_modal(app: &AspApp, cx: &mut Context<AspApp>) -> Option<Div> {
    let Modal::RemoveVault { name, trash, .. } = &app.modal else {
        return None;
    };
    let t = app.theme;
    let trash = *trash;
    let name = name.clone();

    let checkbox = div()
        .id("trash-toggle")
        .flex()
        .items_center()
        .gap(px(8.0))
        .py(px(6.0))
        .cursor_pointer()
        .on_click(cx.listener(|this, _ev, _window, cx| {
            this.toggle_remove_trash();
            cx.notify();
        }))
        .child(
            div()
                .size(px(16.0))
                .rounded(px(4.0))
                .border_1()
                .border_color(if trash { t.accent } else { t.faint })
                .when(trash, |d| d.bg(t.accent))
                .flex()
                .items_center()
                .justify_center()
                .when(trash, |d| d.child(icon("check", px(11.0), gpui::white()))),
        )
        .child(
            div()
                .text_size(px(13.0))
                .text_color(t.text2)
                .child("Also move the folder to the system Trash"),
        );

    let card = div()
        .w(px(424.0))
        .max_w(px(560.0))
        .bg(t.bg)
        .rounded(px(16.0))
        .shadow_lg()
        .p(px(20.0))
        .flex()
        .flex_col()
        .gap(px(12.0))
        .on_mouse_down_out(cx.listener(|this, _ev, _window, cx| {
            this.close_modal();
            cx.notify();
        }))
        .child(
            div()
                .text_size(px(16.0))
                .font_weight(FontWeight(600.0))
                .text_color(t.text)
                .child(format!("Remove “{name}”?")),
        )
        .child(
            div()
                .text_size(px(13.5))
                .text_color(t.text2)
                .child("This stops managing the vault here. Your files stay on disk unless you choose to trash them."),
        )
        .child(checkbox)
        .child(
            div()
                .mt(px(4.0))
                .flex()
                .justify_end()
                .gap(px(8.0))
                .child(
                    div()
                        .id("modal-cancel")
                        .rounded(px(9.0))
                        .px(px(14.0))
                        .py(px(8.0))
                        .border_1()
                        .border_color(t.line)
                        .bg(t.bg)
                        .text_color(t.text2)
                        .text_size(px(13.0))
                        .font_weight(FontWeight(500.0))
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _ev, _window, cx| {
                            this.close_modal();
                            cx.notify();
                        }))
                        .child("Cancel"),
                )
                .child(
                    div()
                        .id("modal-remove")
                        .rounded(px(9.0))
                        .px(px(14.0))
                        .py(px(8.0))
                        .bg(t.delete)
                        .text_color(gpui::white())
                        .text_size(px(13.0))
                        .font_weight(FontWeight(600.0))
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _ev, _window, cx| {
                            this.confirm_remove();
                            cx.notify();
                        }))
                        .child("Remove vault"),
                ),
        );

    // Rendered as the last child of the root (size_full), so a plain absolute
    // overlay paints on top — no `deferred` needed (and `deferred` isn't flushed
    // in the headless single-frame screenshot path).
    Some(
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .bg(t.overlay)
            .flex()
            .items_center()
            .justify_center()
            .child(card),
    )
}
