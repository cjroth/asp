//! Editor screen — data-driven over `AspApp` (see DESIGN_SPEC.md §4–5).

use gpui::{
    div, font, prelude::*, px, Context, Div, Font, FontWeight, Hsla, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, SharedString, StyledText, TextRun, UnderlineStyle,
};

use crate::app::{AspApp, CaretMove};
use crate::icons::icon;
use crate::theme::{self, Theme, FONT_MONO, FONT_SERIF};
use crate::vault::markdown::{self, Inline, Line};
use crate::vault::tree::{self, NodeKind, TreeNode};

pub fn render(app: &AspApp, cx: &mut Context<AspApp>) -> Div {
    let t = app.theme;
    div()
        .size_full()
        .flex()
        .flex_col()
        .bg(t.bg)
        .text_color(t.text)
        // Drag handlers live on the root so they keep firing over any child
        // (sidebar handle, history bar) while a resize drag is in progress.
        .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, window, cx| {
            let mut changed = false;
            if this.dragging_sidebar {
                this.drag_sidebar(f32::from(ev.position.x));
                changed = true;
            }
            if this.dragging_hist {
                let vh = f32::from(window.viewport_size().height);
                this.drag_hist(vh - f32::from(ev.position.y));
                changed = true;
            }
            if changed {
                cx.notify();
            }
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|this, _ev, _window, cx| {
                this.end_drag();
                cx.notify();
            }),
        )
        .child(
            div()
                .flex_1()
                .flex()
                .min_h(px(0.0))
                .child(sidebar(app, cx))
                .child(resize_handle(app, cx))
                .child(editor_pane(app, cx)),
        )
        .child(history_bar(app, cx))
}

fn sidebar(app: &AspApp, cx: &mut Context<AspApp>) -> Div {
    let t = app.theme;
    div()
        .w(px(app.sidebar_w))
        .flex_none()
        .flex()
        .flex_col()
        .bg(t.bg_sub)
        .border_r_1()
        .border_color(t.line)
        .child(vault_switcher(app, cx))
        .child(files_label(app, cx))
        .child(file_tree(app, cx))
}

fn vault_switcher(app: &AspApp, cx: &mut Context<AspApp>) -> impl IntoElement {
    let t = app.theme;
    let initial: SharedString = app
        .vault_name
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default()
        .into();

    let avatar = div()
        .size(px(28.0))
        .rounded(px(8.0))
        .bg(theme::vault_avatar_bg(app.vault_hue))
        .border_1()
        .border_color(theme::vault_avatar_border(app.vault_hue))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(11.2))
        .font_weight(FontWeight(600.0))
        .text_color(theme::vault_monogram(app.vault_hue))
        .child(initial);

    let sync_text = if app.peers > 0 {
        format!("Synced · {} peer{}", app.peers, if app.peers == 1 { "" } else { "s" })
    } else {
        "Synced".to_string()
    };

    div()
        .id("vault-switcher")
        .h(px(47.0))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(11.0))
        .px(px(14.0))
        .cursor_pointer()
        .hover(|s| s.bg(t.line))
        .on_click(cx.listener(|this, _ev, _window, cx| { this.back_to_connect(); cx.notify(); }))
        .child(avatar)
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_size(px(14.0))
                        .font_weight(FontWeight(600.0))
                        .text_color(t.text)
                        .child(app.vault_name.clone()),
                )
                .child(
                    div()
                        .mt(px(2.0))
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .child(div().size(px(6.0)).rounded_full().bg(t.accent))
                        .child(div().text_size(px(11.0)).text_color(t.faint).child(sync_text)),
                ),
        )
        .child(icon("caret-down", px(13.0), t.faint))
}

fn files_label(app: &AspApp, cx: &mut Context<AspApp>) -> Div {
    let t = app.theme;
    let btn = |name: &str| {
        div()
            .size(px(24.0))
            .rounded(px(6.0))
            .flex()
            .items_center()
            .justify_center()
            .hover(|s| s.bg(t.line))
            .child(icon(name, px(16.0), t.faint))
    };
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(1.0))
        .px(px(9.0))
        .pt(px(9.0))
        .pb(px(7.0))
        .child(
            div()
                .flex_1()
                .pl(px(3.0))
                .text_size(px(11.0))
                .font_weight(FontWeight(600.0))
                .text_color(t.faint2)
                .child("FILES"),
        )
        .child(
            btn("plus")
                .id("new-file")
                .cursor_pointer()
                .on_click(cx.listener(|this, _ev, _window, cx| {
                    this.new_file();
                    cx.notify();
                })),
        )
        .child(btn("collapse-all"))
        .child(btn("dots"))
}

fn file_tree(app: &AspApp, cx: &mut Context<AspApp>) -> impl IntoElement {
    let _t = app.theme;
    let nodes = tree::build_tree(app.files.iter().map(|(p, d)| (p.as_str(), *d)));
    let rows = tree::flatten(&nodes, &app.expanded);

    let mut el = div()
        .id("file-tree")
        .flex_1()
        .flex()
        .flex_col()
        .px(px(8.0))
        .pt(px(2.0))
        .pb(px(12.0))
        .overflow_y_scroll();

    for (i, row) in rows.iter().enumerate() {
        el = el.child(tree_row(app, &row.node, row.depth, i, cx));
    }
    el
}

fn tree_row(
    app: &AspApp,
    node: &TreeNode,
    depth: usize,
    idx: usize,
    cx: &mut Context<AspApp>,
) -> impl IntoElement {
    let t = app.theme;
    let is_dir = node.kind == NodeKind::Dir;
    let active = !is_dir && Some(node.path.as_str()) == app.active_path();
    let expanded = app.expanded.get(&node.path).copied().unwrap_or(false);
    let left = 7.0 + depth as f32 * 15.0;

    let leading = div()
        .w(px(16.0))
        .flex()
        .items_center()
        .justify_center()
        .child(if is_dir {
            icon(if expanded { "caret-down" } else { "chevron-right" }, px(11.0), t.faint)
        } else {
            icon("file", px(13.0), if active { t.accent } else { t.faint2 })
        });

    let label_color = if active { t.text } else { t.text2 };
    let weight = if is_dir || active { FontWeight(500.0) } else { FontWeight(400.0) };
    let path = node.path.clone();
    let menu_path = node.path.clone();

    div()
        .id(SharedString::from(format!("row-{idx}")))
        .h(px(29.0))
        .flex()
        .items_center()
        .gap(px(6.0))
        .pl(px(left))
        .pr(px(8.0))
        .rounded(px(7.0))
        .cursor_pointer()
        .when(active, |d| d.bg(t.accent_alpha(0.13)))
        .when(!active, |d| d.hover(|s| s.bg(t.line)))
        .on_click(cx.listener(move |this, _ev, _window, cx| {
            if is_dir {
                this.toggle_dir(&path); cx.notify();
            } else {
                this.select_file(&path); cx.notify();
            }
        }))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, ev: &MouseDownEvent, _window, cx| {
                this.open_file_menu(
                    &menu_path,
                    is_dir,
                    f32::from(ev.position.x),
                    f32::from(ev.position.y),
                );
                cx.notify();
            }),
        )
        .child(leading)
        .child(
            div()
                .flex_1()
                .text_size(px(13.5))
                .font_weight(weight)
                .text_color(label_color)
                .child(node.name.clone()),
        )
}

fn resize_handle(app: &AspApp, cx: &mut Context<AspApp>) -> impl IntoElement {
    let t = app.theme;
    let active = app.dragging_sidebar;
    div()
        .id("sidebar-resize")
        .w(px(7.0))
        .flex_none()
        .flex()
        .justify_center()
        .cursor_col_resize()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _ev, _window, cx| {
                this.start_sidebar_drag();
                cx.notify();
            }),
        )
        .child(
            div()
                .w(px(1.0))
                .h_full()
                .bg(if active { t.accent } else { t.line }),
        )
}

fn editor_pane(app: &AspApp, cx: &mut Context<AspApp>) -> Div {
    let mut pane = div()
        .flex_1()
        .min_w(px(0.0))
        .flex()
        .flex_col()
        .child(tab_bar(app, cx))
        .child(status_row(app));
    if app.is_time_travel() {
        pane = pane.child(time_travel_banner(app, cx));
    }
    pane.child(editor_body(app, cx))
}

fn time_travel_banner(app: &AspApp, cx: &mut Context<AspApp>) -> Div {
    let t = app.theme;
    let when = app
        .playhead
        .map(|ts| crate::vault::history::fmt_full(ts as f64 * 1000.0))
        .unwrap_or_default();
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(12.0))
        .px(px(18.0))
        .py(px(9.0))
        .bg(t.accent_alpha(0.13))
        .border_b_1()
        .border_color(t.accent)
        .child(icon("clock", px(14.0), t.accent))
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .text_size(px(12.5))
                .text_color(t.text2)
                .child(format!("Viewing this vault as it was on {when} · read-only")),
        )
        .child(
            div()
                .id("restore-version")
                .rounded(px(7.0))
                .px(px(12.0))
                .py(px(6.0))
                .bg(t.accent)
                .text_color(t.bg)
                .text_size(px(12.0))
                .font_weight(FontWeight(500.0))
                .cursor_pointer()
                .on_click(cx.listener(|this, _ev, _window, cx| {
                    this.restore_version();
                    cx.notify();
                }))
                .child("Restore this version"),
        )
        .child(
            div()
                .id("return-to-now")
                .rounded(px(7.0))
                .px(px(12.0))
                .py(px(6.0))
                .border_1()
                .border_color(t.line)
                .bg(t.bg)
                .text_color(t.text2)
                .text_size(px(12.0))
                .font_weight(FontWeight(500.0))
                .cursor_pointer()
                .on_click(cx.listener(|this, _ev, _window, cx| {
                    this.return_to_now();
                    cx.notify();
                }))
                .child("Return to now"),
        )
}

fn tab_bar(app: &AspApp, cx: &mut Context<AspApp>) -> Div {
    let t = app.theme;
    let mut strip = div().id("tab-strip").flex_1().min_w(px(0.0)).flex().items_stretch().overflow_x_scroll();
    for (i, tab) in app.tabs.iter().enumerate() {
        let active = Some(tab.as_str()) == app.active_path();
        strip = strip.child(tab_item(app, tab.clone(), active, i, cx));
    }
    div()
        .h(px(48.0))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(10.0))
        .pr(px(16.0))
        .border_b_1()
        .border_color(t.line)
        .child(strip)
        .child(theme_toggle(app, cx))
}

fn theme_toggle(app: &AspApp, cx: &mut Context<AspApp>) -> impl IntoElement {
    let t = app.theme;
    let dark = t.appearance == theme::Appearance::Dark;
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
        .child(icon(if dark { "theme-sun" } else { "theme-moon" }, px(16.0), t.text3))
}

fn tab_item(
    app: &AspApp,
    label: String,
    active: bool,
    idx: usize,
    cx: &mut Context<AspApp>,
) -> Div {
    let t = app.theme;
    let name = crate::vault::format::basename(&label).to_string();
    let select_path = label.clone();
    let close_path = label.clone();
    let menu_path = label.clone();

    div()
        .flex_none()
        .max_w(px(220.0))
        .h_full()
        .flex()
        .items_center()
        .gap(px(7.0))
        .pl(px(12.0))
        .pr(px(8.0))
        .border_r_1()
        .border_color(t.line)
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, ev: &MouseDownEvent, _window, cx| {
                this.open_tab_menu(&menu_path, f32::from(ev.position.x), f32::from(ev.position.y));
                cx.notify();
            }),
        )
        // Drag-to-reorder: press starts the drag, release on another tab drops it.
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _ev, _window, _cx| this.start_tab_drag(idx)),
        )
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(move |this, _ev, _window, cx| {
                this.drop_tab(idx);
                cx.notify();
            }),
        )
        .when(active, |d| d.bg(t.bg).border_t_2().border_color(t.accent))
        .text_size(px(12.5))
        .font_weight(if active { FontWeight(600.0) } else { FontWeight(500.0) })
        .text_color(if active { t.text } else { t.text3 })
        .child(
            div()
                .id(SharedString::from(format!("tab-{idx}")))
                .flex_1()
                .cursor_pointer()
                .on_click(cx.listener(move |this, _ev, _window, cx| {
                    this.select_file(&select_path); cx.notify();
                }))
                .child(name),
        )
        .child(
            div()
                .id(SharedString::from(format!("tab-close-{idx}")))
                .size(px(17.0))
                .rounded(px(4.0))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .on_click(cx.listener(move |this, _ev, _window, cx| {
                    this.close_tab(&close_path); cx.notify();
                }))
                .child(icon("x", px(11.0), if active { t.text } else { t.text3 })),
        )
}

fn status_row(app: &AspApp) -> Div {
    let t = app.theme;
    let words = app.content.split_whitespace().count();
    div()
        .flex_none()
        .flex()
        .items_center()
        .justify_end()
        .gap(px(8.0))
        .px(px(18.0))
        .py(px(7.0))
        .border_b_1()
        .border_color(t.line)
        .child(div().size(px(6.0)).rounded_full().bg(t.create))
        .child(div().text_size(px(11.5)).text_color(t.faint).child("Saved"))
        .child(div().w(px(1.0)).h(px(11.0)).bg(t.line))
        .child(
            div()
                .font_family(FONT_MONO)
                .text_size(px(11.5))
                .text_color(t.faint2)
                .child(format!("{words} words")),
        )
}

fn editor_body(app: &AspApp, cx: &mut Context<AspApp>) -> impl IntoElement {
    let body = if app.editing {
        edit_surface(app, cx).into_any_element()
    } else {
        rendered_surface(app, cx).into_any_element()
    };

    div()
        .id("editor-scroll")
        .flex_1()
        .min_h(px(0.0))
        .flex()
        .justify_center()
        .items_start()
        .overflow_y_scroll()
        .child(body)
}

/// Read-mode: the live markdown render. Clicking enters edit mode.
fn rendered_surface(app: &AspApp, cx: &mut Context<AspApp>) -> impl IntoElement {
    let t = app.theme;
    let mut prose = div()
        .w(px(760.0))
        .max_w_full()
        .flex()
        .flex_col()
        .pt(px(44.0))
        .px(px(40.0))
        .pb(px(140.0))
        .font_family(FONT_SERIF)
        .text_size(px(15.5))
        .text_color(t.text);
    for line in markdown::parse(&app.content) {
        prose = prose.child(render_line(&t, &line));
    }
    let editable = app.active.is_some() && !app.is_time_travel();
    div()
        .id("prose")
        .when(editable, |d| {
            d.cursor_text().on_click(cx.listener(|this, _ev, window, cx| {
                this.begin_edit();
                if let Some(f) = this.focus.clone() {
                    window.focus(&f, cx);
                }
                cx.notify();
            }))
        })
        .child(prose)
}

/// Edit-mode: raw source in the prose font with a caret; receives key input.
fn edit_surface(app: &AspApp, cx: &mut Context<AspApp>) -> impl IntoElement {
    let t = app.theme;
    let text = &app.buffer.text;
    let cur = app.buffer.cursor.min(text.len());
    let before = &text[..cur];
    let cur_line = before.matches('\n').count();

    let mut col = div()
        .w(px(760.0))
        .max_w_full()
        .flex()
        .flex_col()
        .pt(px(44.0))
        .px(px(40.0))
        .pb(px(140.0))
        .font_family(FONT_SERIF)
        .text_size(px(15.5))
        .text_color(t.text);

    let line_start = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
    for (i, line) in text.split('\n').enumerate() {
        if i == cur_line {
            // The active line shows raw source + caret (syntax revealed for editing),
            // mirroring the desktop's live-preview behavior on the focused line.
            let in_line = cur - line_start;
            let (pre, post) = line.split_at(in_line.min(line.len()));
            col = col.child(
                div()
                    .flex()
                    .line_height(px(28.0))
                    .child(div().child(pre.to_string()))
                    .child(div().w(px(2.0)).h(px(22.0)).bg(t.accent).mt(px(3.0)))
                    .child(div().child(post.to_string())),
            );
        } else {
            // Inactive lines render as styled markdown (syntax hidden) — Obsidian-
            // style live preview while typing.
            let parsed = markdown::parse(line).into_iter().next().unwrap_or(Line::Blank);
            col = col.child(render_line(&t, &parsed));
        }
    }

    let focus = app.focus.clone();
    let mut surface = div().id("edit-surface");
    if let Some(f) = focus {
        surface = surface.track_focus(&f);
    }
    surface
        .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _window, cx| {
            handle_key(this, ev);
            cx.notify();
        }))
        .child(col)
}

fn handle_key(app: &mut AspApp, ev: &KeyDownEvent) {
    let ks = &ev.keystroke;
    match ks.key.as_str() {
        "backspace" => app.editor_backspace(),
        "delete" => app.editor_delete(),
        "enter" => app.editor_newline(),
        "left" => app.editor_move(CaretMove::Left),
        "right" => app.editor_move(CaretMove::Right),
        "up" => app.editor_move(CaretMove::Up),
        "down" => app.editor_move(CaretMove::Down),
        "home" => app.editor_move(CaretMove::Home),
        "end" => app.editor_move(CaretMove::End),
        "escape" => app.stop_edit(),
        _ => {
            if !ks.modifiers.control && !ks.modifiers.platform && !ks.modifiers.function {
                if let Some(ch) = &ks.key_char {
                    if !ch.is_empty() {
                        app.type_text(ch);
                    }
                }
            }
        }
    }
}

/// Render one parsed markdown `Line` to a styled gpui block.
fn render_line(t: &Theme, line: &Line) -> Div {
    match line {
        Line::Blank => div().h(px(28.0)),
        Line::Heading { level, spans } => {
            let sz = markdown::heading_size(*level);
            let mt = if *level == 1 { 2.0 } else { 18.0 };
            div()
                .mt(px(mt))
                .mb(px(4.0))
                .text_size(px(sz))
                .line_height(px(sz * 1.3))
                .child(styled(t, spans, Hsla::from(t.text), FontWeight(600.0), false))
        }
        Line::Quote(spans) => div()
            .border_l_2()
            .border_color(t.accent)
            .pl(px(12.0))
            .line_height(px(28.0))
            .text_color(t.text2)
            .child(styled(t, spans, Hsla::from(t.text2), FontWeight::NORMAL, true)),
        Line::Hr => div().my(px(8.0)).border_b_1().border_color(t.line).h(px(1.0)),
        Line::Task { indent, done, spans } => {
            let box_color = if *done { t.accent } else { t.faint };
            let checkbox = div()
                .size(px(15.0))
                .rounded(px(4.0))
                .border_1()
                .border_color(box_color)
                .when(*done, |d| d.bg(t.accent))
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .when(*done, |d| d.child(icon("check", px(11.0), gpui::white())));
            div()
                .ml(px(*indent as f32 * 8.5))
                .flex()
                .items_start()
                .gap(px(8.0))
                .line_height(px(28.0))
                .child(div().pt(px(5.0)).child(checkbox))
                .child(styled(
                    t,
                    spans,
                    if *done { Hsla::from(t.faint) } else { Hsla::from(t.text) },
                    FontWeight::NORMAL,
                    false,
                ))
        }
        Line::Bullet { indent, spans } => div()
            .ml(px(*indent as f32 * 8.5))
            .flex()
            .gap(px(8.0))
            .line_height(px(28.0))
            .child(div().text_color(t.faint).child("•"))
            .child(styled(t, spans, Hsla::from(t.text), FontWeight::NORMAL, false)),
        Line::Ordered { indent, number, spans } => div()
            .ml(px(*indent as f32 * 8.5))
            .flex()
            .gap(px(6.0))
            .line_height(px(28.0))
            .child(
                div()
                    .text_color(t.accent)
                    .font_weight(FontWeight(500.0))
                    .child(number.clone()),
            )
            .child(styled(t, spans, Hsla::from(t.text), FontWeight::NORMAL, false)),
        Line::Code { text, lang } => {
            let base = div()
                .font_family(FONT_MONO)
                .text_size(px(13.0))
                .line_height(px(22.0))
                .px(px(12.0))
                .bg(t.bg_input);
            match lang {
                Some(l) if !l.is_empty() => base.child(code_line(*t, text, l)),
                _ => base
                    .text_color(t.faint)
                    .child(if text.is_empty() { " ".to_string() } else { text.clone() }),
            }
        }
        Line::Para(spans) => div()
            .line_height(px(28.0))
            .child(styled(t, spans, Hsla::from(t.text), FontWeight::NORMAL, false)),
    }
}

/// Build a syntax-highlighted `StyledText` for one code line.
fn code_line(t: Theme, text: &str, lang: &str) -> StyledText {
    use crate::vault::highlight::{highlight, Tok};
    let mono = font(FONT_MONO);
    let color = |tok: Tok| -> Hsla {
        match tok {
            Tok::Keyword => Hsla::from(t.accent),
            Tok::Str => Hsla::from(t.create),
            Tok::Comment => Hsla::from(t.faint),
            Tok::Number => Hsla::from(t.rename),
            Tok::Type => gpui::hsla(188.0 / 360.0, 0.5, 0.42, 1.0),
            Tok::Plain => Hsla::from(t.text2),
        }
    };
    let mut s = String::new();
    let mut runs: Vec<TextRun> = Vec::new();
    for (tok, txt) in highlight(text, lang) {
        let len = txt.len();
        if len == 0 {
            continue;
        }
        s.push_str(&txt);
        runs.push(TextRun { len, font: mono.clone(), color: color(tok), ..Default::default() });
    }
    if s.is_empty() {
        s.push(' ');
        runs.push(TextRun { len: 1, font: mono.clone(), color: Hsla::from(t.text2), ..Default::default() });
    }
    StyledText::new(s).with_runs(runs)
}

/// Build a `StyledText` from inline spans with per-run font/weight/style/color.
fn styled(
    t: &Theme,
    spans: &[Inline],
    base: Hsla,
    base_weight: FontWeight,
    base_italic: bool,
) -> StyledText {
    let serif = |weight: FontWeight, italic: bool| -> Font {
        let mut f = font(FONT_SERIF);
        f.weight = weight;
        if italic {
            f.style = gpui::FontStyle::Italic;
        }
        f
    };
    let mut s = String::new();
    let mut runs: Vec<TextRun> = Vec::new();
    let mut push = |text: String, fnt: Font, color: Hsla, bg: Option<Hsla>, underline: bool| {
        let len = text.len();
        if len == 0 {
            return;
        }
        s.push_str(&text);
        runs.push(TextRun {
            len,
            font: fnt,
            color,
            background_color: bg,
            underline: underline
                .then(|| UnderlineStyle { thickness: px(1.0), color: Some(color), wavy: false }),
            strikethrough: None,
        });
    };

    for sp in spans {
        match sp {
            Inline::Text(x) => push(x.clone(), serif(base_weight, base_italic), base, None, false),
            Inline::Bold(x) => push(x.clone(), serif(FontWeight::BOLD, base_italic), base, None, false),
            Inline::Italic(x) => push(x.clone(), serif(base_weight, true), base, None, false),
            Inline::Code(x) => {
                push(x.clone(), font(FONT_MONO), Hsla::from(t.text), Some(Hsla::from(t.bg_input)), false)
            }
            Inline::Link { text, url: _ } => {
                push(text.clone(), serif(base_weight, base_italic), Hsla::from(t.accent), None, true)
            }
            Inline::Image { alt, url: _ } => {
                push(format!("🖼 {alt}"), serif(base_weight, base_italic), Hsla::from(t.faint), None, false)
            }
        }
    }
    if s.is_empty() {
        // empty line still needs a (zero-content) run to lay out a blank line
        s.push(' ');
        runs.push(TextRun { len: 1, font: serif(base_weight, base_italic), color: base, ..Default::default() });
    }
    StyledText::new(s).with_runs(runs)
}

fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as f64)
        .unwrap_or(0.0)
}

fn kind_color(t: &Theme, kind: &str) -> gpui::Rgba {
    match kind {
        "create" => t.create,
        "rename" => t.rename,
        "delete" | "reclass" => t.delete,
        _ => t.edit,
    }
}

fn history_bar(app: &AspApp, cx: &mut Context<AspApp>) -> Div {
    let t = app.theme;
    let now = now_ms();
    let label = match app.playhead {
        Some(ts) => crate::vault::history::fmt_full(ts as f64 * 1000.0),
        None => "now".to_string(),
    };
    let status = div()
        .h(px(38.0))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(8.0))
        .pl(px(15.0))
        .pr(px(9.0))
        .child(icon("clock", px(14.0), t.faint))
        .child(div().text_size(px(12.0)).text_color(t.text2).child(format!("History · {label}")))
        .child(div().flex_1())
        .child(round_btn(app, "minus"))
        .child(round_btn(app, "plus"));

    let mut ticks = div().relative().flex_1().h_full();

    if app.history_events.is_empty() {
        // Decorative ticks for the fixture (no engine / no history yet).
        let positions = [0.08f32, 0.21, 0.34, 0.52, 0.67, 0.79, 0.93];
        let colors = [t.create, t.edit, t.edit, t.rename, t.edit, t.delete, t.edit];
        for (i, p) in positions.iter().enumerate() {
            ticks = ticks.child(
                div()
                    .absolute()
                    .top(px(14.0))
                    .left(gpui::relative(*p))
                    .w(px(2.0))
                    .h(px(34.0))
                    .rounded(px(1.0))
                    .bg(colors[i % colors.len()]),
            );
        }
    } else {
        // Real, clickable event ticks positioned by the ported timeline geometry.
        let view = crate::vault::history::default_view(now);
        for (i, e) in app.history_events.iter().enumerate() {
            let pct = crate::vault::history::to_pct(e.ts, view);
            if !(0.0..=100.0).contains(&pct) {
                continue;
            }
            let ts_secs = (e.ts / 1000.0) as i64;
            ticks = ticks.child(
                div()
                    .id(SharedString::from(format!("tick-{i}")))
                    .absolute()
                    .top(px(10.0))
                    .left(gpui::relative(pct as f32 / 100.0))
                    .w(px(6.0))
                    .h(px(40.0))
                    .flex()
                    .justify_center()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _ev, _window, cx| {
                        this.time_travel_to(ts_secs);
                        cx.notify();
                    }))
                    .child(div().w(px(2.0)).h_full().rounded(px(1.0)).bg(kind_color(&t, &e.kind))),
            );
        }
    }

    // Playhead line — at the time-travel instant, else at "now".
    let view = crate::vault::history::default_view(now);
    let head_pct = crate::vault::history::to_pct(
        app.playhead.map(|ts| ts as f64 * 1000.0).unwrap_or(now),
        view,
    );
    let head_pct = head_pct.clamp(0.0, 100.0) as f32 / 100.0;
    ticks = ticks.child(
        div()
            .absolute()
            .top(px(8.0))
            .left(gpui::relative(head_pct))
            .w(px(2.0))
            .h(px(46.0))
            .rounded(px(1.0))
            .bg(t.accent),
    );

    let track = div().flex_1().flex().mx(px(16.0)).mb(px(11.0)).child(ticks);

    let handle = div()
        .id("hist-resize")
        .h(px(5.0))
        .w_full()
        .flex_none()
        .cursor_row_resize()
        .when(app.dragging_hist, |d| d.bg(t.accent))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _ev, _window, cx| {
                this.start_hist_drag();
                cx.notify();
            }),
        );

    div()
        .h(px(app.hist_bar_h))
        .flex_none()
        .flex()
        .flex_col()
        .bg(t.bg_sub)
        .border_t_1()
        .border_color(t.line)
        .child(handle)
        .child(status)
        .child(track)
}

fn round_btn(app: &AspApp, name: &str) -> Div {
    let t = app.theme;
    div()
        .size(px(24.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(t.line)
        .bg(t.bg)
        .flex()
        .items_center()
        .justify_center()
        .child(icon(name, px(14.0), t.text3))
}
