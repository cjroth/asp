//! Floating overlays — context menus (anchored) + modal dialogs (centered).
//! See DESIGN_SPEC.md §5 (menus/modals).

use gpui::{
    anchored, deferred, div, point, prelude::*, px, Context, Div, FontWeight, KeyDownEvent,
    PathPromptOptions,
};

use crate::app::{AspApp, Menu, Modal};
use crate::icons::icon;
use crate::theme::Theme;

/// A menu row.
fn mitem(t: &Theme, label: &str, icon_name: &'static str, danger: bool) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(10.0))
        .px(px(9.0))
        .py(px(8.0))
        .rounded(px(8.0))
        .text_size(px(13.0))
        .text_color(if danger { t.delete } else { t.text })
        .hover(|s| s.bg(t.bg_sub))
        .cursor_pointer()
        .child(icon(icon_name, px(15.0), if danger { t.delete } else { t.faint }))
        .child(label.to_string())
}

/// A menu card shell.
fn menu_card(t: &Theme, w: f32) -> Div {
    div()
        .w(px(w))
        .bg(t.bg)
        .border_1()
        .border_color(t.line)
        .rounded(px(12.0))
        .shadow_lg()
        .p(px(6.0))
        .flex()
        .flex_col()
        .gap(px(2.0))
}

/// Float a card at (x, y) via an anchored deferred layer.
fn floating(x: f32, y: f32, card: Div) -> Div {
    div().child(deferred(anchored().position(point(px(x), px(y))).child(card)).priority(2))
}

/// Tab context menu (right-click a tab).
pub fn tab_menu(app: &AspApp, cx: &mut Context<AspApp>) -> Option<Div> {
    let Menu::Tab { path, x, y } = &app.menu else {
        return None;
    };
    let t = app.theme;
    let p = path.clone();
    let close_item = |id: &'static str, label: &str, f: fn(&mut AspApp, &str)| {
        let p = p.clone();
        mitem(&t, label, "x", false).id(id).on_click(cx.listener(move |this, _ev, _window, cx| {
            f(this, &p);
            this.close_menu();
            cx.notify();
        }))
    };
    let card = menu_card(&t, 200.0)
        .on_mouse_down_out(cx.listener(|this, _ev, _window, cx| {
            this.close_menu();
            cx.notify();
        }))
        .child(close_item("tm-close", "Close", |a, p| a.close_tab(p)))
        .child(close_item("tm-others", "Close Others", |a, p| a.close_others(p)))
        .child(close_item("tm-left", "Close to the Left", |a, p| a.close_to_left(p)))
        .child(close_item("tm-right", "Close to the Right", |a, p| a.close_to_right(p)))
        .child(div().h(px(1.0)).my(px(4.0)).mx(px(6.0)).bg(t.line))
        .child(
            mitem(&t, "Close All", "x", false).id("tm-all").on_click(cx.listener(
                |this, _ev, _window, cx| {
                    this.close_all_tabs();
                    this.close_menu();
                    cx.notify();
                },
            )),
        );
    Some(floating(*x, *y, card))
}

/// File-tree context menu (right-click a file/folder).
pub fn file_menu(app: &AspApp, cx: &mut Context<AspApp>) -> Option<Div> {
    let Menu::File { path, is_dir, x, y } = &app.menu else {
        return None;
    };
    let t = app.theme;
    let p = path.clone();
    let is_dir = *is_dir;
    let mut card = menu_card(&t, 190.0).on_mouse_down_out(cx.listener(|this, _ev, _window, cx| {
        this.close_menu();
        cx.notify();
    }));
    card = card.child({
        mitem(&t, "New file", "new-file", false).id("fm-new").on_click(cx.listener(
            |this, _ev, _window, cx| {
                this.new_file();
                this.close_menu();
                cx.notify();
            },
        ))
    });
    if !is_dir {
        let pd = p.clone();
        card = card
            .child(div().h(px(1.0)).my(px(4.0)).mx(px(6.0)).bg(t.line))
            .child(mitem(&t, "Delete", "trash", true).id("fm-del").on_click(cx.listener(
                move |this, _ev, _window, cx| {
                    this.delete_file(&pd);
                    this.close_menu();
                    cx.notify();
                },
            )));
    }
    Some(floating(*x, *y, card))
}

/// The "share vault" modal — shows the connection ticket.

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
        .child({
            let (id, name) = (id.clone(), name.clone());
            item("Share this vault…", "share", false).id("menu-share").on_click(cx.listener(
                move |this, _ev, _window, cx| {
                    this.open_share(&id, &name);
                    cx.notify();
                },
            ))
        })
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

/// The "share vault" modal — shows the connection ticket.
pub fn share_modal(app: &AspApp, cx: &mut Context<AspApp>) -> Option<Div> {
    let Modal::ShareVault { name, ticket } = &app.modal else {
        return None;
    };
    let t = app.theme;
    let name = name.clone();
    let ticket_text = ticket
        .clone()
        .unwrap_or_else(|| "Could not start listening.".to_string());

    let card = div()
        .w(px(424.0))
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
                .child(format!("Share “{name}”")),
        )
        .child(
            div()
                .text_size(px(13.5))
                .text_color(t.text2)
                .child("Send this connection code to a peer. They paste it into “Connect Vault” to sync."),
        )
        .child(
            div()
                .font_family(crate::theme::FONT_MONO)
                .text_size(px(12.0))
                .text_color(t.text)
                .bg(t.bg_input)
                .border_1()
                .border_color(t.line)
                .rounded(px(9.0))
                .p(px(12.0))
                .child(ticket_text),
        )
        .child(
            div().mt(px(4.0)).flex().justify_end().child(
                div()
                    .id("share-done")
                    .rounded(px(9.0))
                    .px(px(14.0))
                    .py(px(8.0))
                    .bg(t.text)
                    .text_color(t.bg)
                    .text_size(px(13.0))
                    .font_weight(FontWeight(600.0))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _ev, _window, cx| {
                        this.close_modal();
                        cx.notify();
                    }))
                    .child("Done"),
            ),
        );

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

/// The "connect a vault" modal — paste a ticket, pick a destination, clone.
pub fn connect_modal(app: &AspApp, cx: &mut Context<AspApp>) -> Option<Div> {
    let Modal::ConnectVault { buf } = &app.modal else {
        return None;
    };
    let t = app.theme;
    let text = buf.text.clone();
    let empty = text.trim().is_empty();

    // The ticket field: monospace text + caret; focusable; handles typing + paste.
    let mut field = div()
        .id("connect-field")
        .min_h(px(40.0))
        .w_full()
        .font_family(crate::theme::FONT_MONO)
        .text_size(px(12.0))
        .text_color(t.text)
        .bg(t.bg_input)
        .border_1()
        .border_color(t.accent)
        .rounded(px(9.0))
        .p(px(12.0))
        .flex()
        .items_center();
    if let Some(f) = app.focus.clone() {
        field = field.track_focus(&f);
    }
    let field = field
        .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _window, cx| {
            let ks = &ev.keystroke;
            match ks.key.as_str() {
                "backspace" => this.connect_backspace(),
                "v" if ks.modifiers.platform || ks.modifiers.control => {
                    if let Some(item) = cx.read_from_clipboard() {
                        if let Some(txt) = item.text() {
                            this.connect_type(&txt);
                        }
                    }
                }
                _ => {
                    if !ks.modifiers.control && !ks.modifiers.platform {
                        if let Some(c) = &ks.key_char {
                            this.connect_type(c);
                        }
                    }
                }
            }
            cx.notify();
        }))
        .child(if empty {
            div().text_color(t.faint).child("Paste a connection code…")
        } else {
            div().child(text)
        })
        .child(div().w(px(2.0)).h(px(16.0)).bg(t.accent));

    let card = div()
        .w(px(424.0))
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
                .child("Connect a vault"),
        )
        .child(
            div()
                .text_size(px(13.5))
                .text_color(t.text2)
                .child("Paste the code a peer shared, then choose where to keep the vault."),
        )
        .child(field)
        .child(
            div()
                .mt(px(4.0))
                .flex()
                .justify_end()
                .gap(px(8.0))
                .child(
                    div()
                        .id("connect-cancel")
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
                        .id("connect-go")
                        .rounded(px(9.0))
                        .px(px(14.0))
                        .py(px(8.0))
                        .bg(t.text)
                        .text_color(t.bg)
                        .text_size(px(13.0))
                        .font_weight(FontWeight(600.0))
                        .cursor_pointer()
                        .on_click(cx.listener(|_this, _ev, _window, cx| {
                            let rx = cx.prompt_for_paths(PathPromptOptions {
                                files: false,
                                directories: true,
                                multiple: false,
                                prompt: Some("Choose where to clone the vault".into()),
                            });
                            cx.spawn(async move |this, cx| {
                                if let Ok(Ok(Some(paths))) = rx.await {
                                    if let Some(p) = paths.into_iter().next() {
                                        this.update(cx, |this, cx| {
                                            this.connect_confirm(&p);
                                            cx.notify();
                                        })
                                        .ok();
                                    }
                                }
                            })
                            .detach();
                        }))
                        .child("Connect"),
                ),
        );

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
