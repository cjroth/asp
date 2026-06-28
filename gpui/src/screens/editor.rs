//! Editor screen — sidebar (vault switcher + file tree + footer), tab bar,
//! live editor pane, and the history bar. Skeleton with fixture data; wired to
//! the engine later. See DESIGN_SPEC.md §4–5.

use std::collections::HashMap;

use gpui::{div, prelude::*, px, FontWeight, SharedString, Window};

use crate::icons::icon;
use crate::theme::{self, Theme, FONT_MONO, FONT_SERIF};
use crate::vault::tree::{self, NodeKind, TreeNode};

/// Fixture vault for visual checks.
pub struct EditorScreen {
    pub theme: Theme,
    pub vault_name: SharedString,
    pub hue: f32,
    pub peers: usize,
    pub files: Vec<(SharedString, bool)>, // (path, is_dir)
    pub expanded: HashMap<String, bool>,
    pub tabs: Vec<SharedString>,
    pub active: SharedString,
    pub title: SharedString,
    pub body: Vec<SharedString>,
}

impl EditorScreen {
    pub fn fixture(theme: Theme) -> Self {
        let files: Vec<(SharedString, bool)> = vec![
            ("README.md".into(), false),
            ("drafts".into(), true),
            ("drafts/launch-post.md".into(), false),
            ("notes".into(), true),
            ("notes/ideas.md".into(), false),
            ("notes/todo.md".into(), false),
            ("changelog.md".into(), false),
        ];
        let mut expanded = HashMap::new();
        expanded.insert("notes".to_string(), true);
        expanded.insert("drafts".to_string(), true);
        EditorScreen {
            theme,
            vault_name: "Research Notes".into(),
            hue: theme::VAULT_HUES[0],
            peers: 2,
            files,
            expanded,
            tabs: vec!["README.md".into(), "ideas.md".into(), "todo.md".into()],
            active: "README.md".into(),
            title: "Research Notes".into(),
            body: vec![
                "A local-first vault for research. Everything here syncs peer-to-peer with end-to-end encryption — no servers in between.".into(),
                "The editor is a live WYSIWYG surface: Markdown renders as you type, with the source preserved byte-for-byte underneath.".into(),
                "Use the history scrubber below to travel back through every edit and restore any past version of a file.".into(),
            ],
        }
    }
}

impl Render for EditorScreen {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(t.bg)
            .text_color(t.text)
            .child(
                // Top content area: sidebar | handle | editor pane.
                div()
                    .flex_1()
                    .flex()
                    .min_h(px(0.0))
                    .child(self.sidebar())
                    .child(self.resize_handle())
                    .child(self.editor_pane()),
            )
            .child(self.history_bar())
    }
}

impl EditorScreen {
    fn sidebar(&self) -> impl IntoElement {
        let t = self.theme;
        div()
            .w(px(266.0))
            .flex_none()
            .flex()
            .flex_col()
            .bg(t.bg_sub)
            .border_r_1()
            .border_color(t.line)
            .child(self.vault_switcher())
            .child(self.files_label())
            .child(self.file_tree())
    }

    fn vault_switcher(&self) -> impl IntoElement {
        let t = self.theme;
        let avatar = div()
            .size(px(28.0))
            .rounded(px(8.0))
            .bg(theme::vault_avatar_bg(self.hue))
            .border_1()
            .border_color(theme::vault_avatar_border(self.hue))
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(11.2))
            .font_weight(FontWeight(600.0))
            .text_color(theme::vault_monogram(self.hue))
            .child(
                self.vault_name
                    .chars()
                    .next()
                    .map(|c| c.to_uppercase().to_string())
                    .unwrap_or_default(),
            );

        let sync_text = if self.peers > 0 {
            format!("Synced · {} peer{}", self.peers, if self.peers == 1 { "" } else { "s" })
        } else {
            "Synced".to_string()
        };

        div()
            .h(px(47.0))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(11.0))
            .px(px(14.0))
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
                            .child(self.vault_name.clone()),
                    )
                    .child(
                        div()
                            .mt(px(2.0))
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .child(div().size(px(6.0)).rounded_full().bg(t.accent))
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(t.faint)
                                    .child(sync_text),
                            ),
                    ),
            )
            .child(icon("caret-down", px(13.0), t.faint))
    }

    fn files_label(&self) -> impl IntoElement {
        let t = self.theme;
        let btn = |name: &str| {
            div()
                .size(px(24.0))
                .rounded(px(6.0))
                .flex()
                .items_center()
                .justify_center()
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
            .child(btn("plus"))
            .child(btn("collapse-all"))
            .child(btn("dots"))
    }

    fn file_tree(&self) -> impl IntoElement {
        let t = self.theme;
        let nodes = tree::build_tree(self.files.iter().map(|(p, d)| (p.as_str(), *d)));
        let rows = tree::flatten(&nodes, &self.expanded);

        let mut tree_el = div()
            .flex_1()
            .flex()
            .flex_col()
            .px(px(8.0))
            .pt(px(2.0))
            .pb(px(12.0));

        for row in rows {
            let node = &row.node;
            let is_dir = node.kind == NodeKind::Dir;
            let active = !is_dir && node.path == self.active_path();
            let expanded = self.expanded.get(&node.path).copied().unwrap_or(false);
            tree_el = tree_el.child(self.tree_row(node, row.depth, is_dir, active, expanded));
        }
        tree_el
    }

    fn active_path(&self) -> String {
        // fixture: README.md is the active file's path
        "README.md".to_string()
    }

    fn tree_row(
        &self,
        node: &TreeNode,
        depth: usize,
        is_dir: bool,
        active: bool,
        expanded: bool,
    ) -> impl IntoElement {
        let t = self.theme;
        let left = 7.0 + depth as f32 * 15.0;

        // Chevron (folders) or file icon, in a 16px column.
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

        let label_color = if active {
            t.text
        } else if is_dir {
            t.text2
        } else {
            t.text2
        };
        let weight = if is_dir || active { FontWeight(500.0) } else { FontWeight(400.0) };

        div()
            .h(px(29.0))
            .flex()
            .items_center()
            .gap(px(6.0))
            .pl(px(left))
            .pr(px(8.0))
            .rounded(px(7.0))
            .when(active, |d| d.bg(t.accent_alpha(0.13)))
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

    fn resize_handle(&self) -> impl IntoElement {
        let t = self.theme;
        div()
            .w(px(7.0))
            .flex_none()
            .flex()
            .justify_center()
            .child(div().w(px(1.0)).h_full().bg(t.line))
    }

    fn editor_pane(&self) -> impl IntoElement {
        div()
            .flex_1()
            .min_w(px(0.0))
            .flex()
            .flex_col()
            .child(self.tab_bar())
            .child(self.status_row())
            .child(self.editor_body())
    }

    fn tab_bar(&self) -> impl IntoElement {
        let t = self.theme;
        let mut strip = div().flex_1().min_w(px(0.0)).flex().items_stretch();
        for tab in &self.tabs {
            let active = *tab == self.active;
            strip = strip.child(self.tab(tab.clone(), active));
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
            .child(
                div()
                    .size(px(28.0))
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(t.line)
                    .bg(t.bg)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon("theme-moon", px(16.0), t.text3)),
            )
    }

    fn tab(&self, label: SharedString, active: bool) -> impl IntoElement {
        let t = self.theme;
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
            .when(active, |d| d.bg(t.bg).border_t_2().border_color(t.accent))
            .text_size(px(12.5))
            .font_weight(if active { FontWeight(600.0) } else { FontWeight(500.0) })
            .text_color(if active { t.text } else { t.text3 })
            .child(div().flex_1().child(label))
            .child(
                div()
                    .size(px(17.0))
                    .rounded(px(4.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon("x", px(11.0), if active { t.text } else { t.text3 })),
            )
    }

    fn status_row(&self) -> impl IntoElement {
        let t = self.theme;
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
                    .child("84 words"),
            )
    }

    fn editor_body(&self) -> impl IntoElement {
        let t = self.theme;
        let mut prose = div()
            .w(px(760.0))
            .max_w_full()
            .flex()
            .flex_col()
            .pt(px(44.0))
            .px(px(40.0))
            .pb(px(140.0))
            .font_family(FONT_SERIF)
            .text_color(t.text)
            .child(
                div()
                    .text_size(px(30.0))
                    .font_weight(FontWeight(600.0))
                    .line_height(px(38.0))
                    .mb(px(14.0))
                    .child(self.title.clone()),
            );
        for p in &self.body {
            prose = prose.child(
                div()
                    .text_size(px(15.5))
                    .line_height(px(28.0))
                    .mb(px(16.0))
                    .child(p.clone()),
            );
        }

        div()
            .flex_1()
            .min_h(px(0.0))
            .flex()
            .justify_center()
            .items_start()
            .overflow_hidden()
            .child(prose)
    }

    fn history_bar(&self) -> impl IntoElement {
        let t = self.theme;
        // Status row: clock + label + zoom controls.
        let status = div()
            .h(px(38.0))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.0))
            .pl(px(15.0))
            .pr(px(9.0))
            .child(icon("clock", px(14.0), t.faint))
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(t.text2)
                    .child("History · now"),
            )
            .child(div().flex_1())
            .child(self.round_btn("minus"))
            .child(self.round_btn("plus"));

        // Track with a few event ticks + a playhead at the right (now).
        let mut ticks = div().relative().flex_1().h_full();
        let positions = [0.08f32, 0.21, 0.34, 0.52, 0.67, 0.79, 0.93];
        let colors = [t.create, t.edit, t.edit, t.rename, t.edit, t.delete, t.edit];
        for (i, p) in positions.iter().enumerate() {
            ticks = ticks.child(
                div()
                    .absolute()
                    .top(px(14.0))
                    .left(relative(*p))
                    .w(px(2.0))
                    .h(px(34.0))
                    .rounded(px(1.0))
                    .bg(colors[i % colors.len()]),
            );
        }
        // Playhead at "now" (far right).
        ticks = ticks.child(
            div()
                .absolute()
                .top(px(8.0))
                .right(px(0.0))
                .w(px(2.0))
                .h(px(46.0))
                .rounded(px(1.0))
                .bg(t.accent),
        );

        let track = div()
            .flex_1()
            .flex()
            .mx(px(16.0))
            .mb(px(11.0))
            .child(ticks);

        div()
            .h(px(150.0))
            .flex_none()
            .flex()
            .flex_col()
            .bg(t.bg_sub)
            .border_t_1()
            .border_color(t.line)
            .child(status)
            .child(track)
    }

    fn round_btn(&self, name: &str) -> impl IntoElement {
        let t = self.theme;
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
}

use gpui::relative;
