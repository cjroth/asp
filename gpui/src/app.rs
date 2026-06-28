//! The root view of the aspgui vault editor. Holds screen/selection state and
//! drives the `Backend`. Both real UI handlers and the headless screenshot
//! driver call the same `pub` methods (open_vault, select_file, …).

use crate::backend::Backend;
use crate::theme::Theme;
use asp_desktop_engine::{FileEntry, HistEvent, VaultInfo};
use gpui::prelude::*;
use gpui::{div, px, relative, rgb, Context, FocusHandle, FontWeight, KeyDownEvent, Rgba, SharedString, Window};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Copy, PartialEq)]
pub enum Screen {
    Connect,
    Editor,
}

pub struct AspApp {
    pub backend: Backend,
    pub theme: Theme,
    pub screen: Screen,
    pub vaults: Vec<VaultInfo>,
    pub current_vault: Option<String>,
    pub files: Vec<FileEntry>,
    pub current_path: Option<String>,
    pub content: String,
    pub saving: bool,
    pub history: Vec<HistEvent>,
    /// `None` = viewing the live (now) state; `Some(ts)` = time-traveling.
    pub playhead_ts: Option<i64>,
    pub share_open: bool,
    pub share_code: Option<String>,
    /// Source-edit mode: when true the content pane shows the raw Markdown
    /// source with a caret and accepts keyboard input; when false it renders.
    pub editing: bool,
    /// Caret position as a byte offset into `content` (always on a char boundary).
    pub cursor: usize,
    pub focus: FocusHandle,
    /// Scroll state for the virtualized file list (only visible rows render).
    pub file_scroll: gpui::UniformListScrollHandle,
}

impl AspApp {
    pub fn new(backend: Backend, cx: &mut Context<Self>) -> Self {
        let vaults = backend.list_vaults();
        // Reopen previously-saved vaults *off* the main thread. Their rescan can
        // take tens of seconds (large vaults / slow mounts); doing it
        // synchronously before the window opens would block the UI from ever
        // appearing. The engine registers each vault as soon as it finishes
        // loading, so we poll `refresh_vaults()` while the rescan runs — each
        // vault row appears (and becomes clickable) the moment it is ready,
        // instead of all of them blinking in only when the last one is done.
        cx.spawn({
            let backend = backend.clone();
            async move |this, cx| {
                let done = Arc::new(AtomicBool::new(false));
                cx.background_executor()
                    .spawn({
                        let backend = backend.clone();
                        let done = done.clone();
                        async move {
                            backend.reopen_saved();
                            done.store(true, Ordering::SeqCst);
                        }
                    })
                    .detach();
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(300))
                        .await;
                    let finished = done.load(Ordering::SeqCst);
                    if this
                        .update(cx, |app, cx| {
                            app.refresh_vaults();
                            cx.notify();
                        })
                        .is_err()
                        || finished
                    {
                        break;
                    }
                }
            }
        })
        .detach();
        AspApp {
            backend,
            focus: cx.focus_handle(),
            editing: false,
            cursor: 0,
            theme: Theme::light(),
            screen: Screen::Connect,
            vaults,
            current_vault: None,
            files: Vec::new(),
            current_path: None,
            content: String::new(),
            saving: false,
            history: Vec::new(),
            playhead_ts: None,
            share_open: false,
            share_code: None,
            file_scroll: gpui::UniformListScrollHandle::new(),
        }
    }

    /// Open the share sheet, generating (or reusing) a share ticket by enabling
    /// connections on the vault — forwards to `set_allow_connections`.
    /// Enter source-edit mode for the open file (disabled while time-traveling).
    pub fn enter_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.current_path.is_none() || self.playhead_ts.is_some() {
            return;
        }
        self.editing = true;
        self.cursor = self.content.len();
        window.focus(&self.focus, cx);
        cx.notify();
    }

    pub fn exit_edit(&mut self, cx: &mut Context<Self>) {
        self.editing = false;
        cx.notify();
    }

    /// Insert text at the caret (used by real key input and the screenshot driver).
    pub fn type_str(&mut self, s: &str, cx: &mut Context<Self>) {
        if !self.editing {
            return;
        }
        self.content.insert_str(self.cursor, s);
        self.cursor += s.len();
        self.save_content();
        cx.notify();
    }

    fn save_content(&mut self) {
        if let (Some(id), Some(path)) = (self.current_vault.clone(), self.current_path.clone()) {
            let _ = self.backend.write_file(&id, &path, &self.content);
            // Refresh history so a new edit event appears on the timeline.
            self.history = self.backend.history(&id).unwrap_or_default();
        }
    }

    /// Keyboard handling for source-edit mode.
    pub fn on_key(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.editing {
            return;
        }
        let key = ev.keystroke.key.as_str();
        let modified = ev.keystroke.modifiers.control || ev.keystroke.modifiers.platform;
        let mut changed = false;
        match key {
            "escape" => {
                self.exit_edit(cx);
                return;
            }
            "backspace" => {
                if self.cursor > 0 {
                    let prev = self.content[..self.cursor]
                        .chars()
                        .next_back()
                        .map(|c| c.len_utf8())
                        .unwrap_or(0);
                    let start = self.cursor - prev;
                    self.content.replace_range(start..self.cursor, "");
                    self.cursor = start;
                    changed = true;
                }
            }
            "enter" => {
                self.content.insert(self.cursor, '\n');
                self.cursor += 1;
                changed = true;
            }
            "left" => {
                self.cursor -= self.content[..self.cursor]
                    .chars()
                    .next_back()
                    .map(|c| c.len_utf8())
                    .unwrap_or(0);
            }
            "right" => {
                if self.cursor < self.content.len() {
                    self.cursor += self.content[self.cursor..]
                        .chars()
                        .next()
                        .map(|c| c.len_utf8())
                        .unwrap_or(0);
                }
            }
            "home" => self.cursor = self.line_start(self.cursor),
            "end" => self.cursor = self.line_end(self.cursor),
            "up" => self.move_line(true),
            "down" => self.move_line(false),
            _ => {
                if !modified {
                    if let Some(ch) = ev.keystroke.key_char.as_ref() {
                        if !ch.is_empty() && !ch.chars().any(|c| c.is_control()) {
                            self.content.insert_str(self.cursor, ch);
                            self.cursor += ch.len();
                            changed = true;
                        }
                    }
                }
            }
        }
        if changed {
            self.save_content();
        }
        cx.notify();
    }

    fn line_start(&self, pos: usize) -> usize {
        self.content[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0)
    }

    fn line_end(&self, pos: usize) -> usize {
        self.content[pos..]
            .find('\n')
            .map(|i| pos + i)
            .unwrap_or(self.content.len())
    }

    fn move_line(&mut self, up: bool) {
        let start = self.line_start(self.cursor);
        let col = self.cursor - start;
        if up {
            if start == 0 {
                return;
            }
            let prev_start = self.line_start(start - 1);
            let prev_len = (start - 1) - prev_start;
            self.cursor = prev_start + col.min(prev_len);
        } else {
            let end = self.line_end(self.cursor);
            if end >= self.content.len() {
                return;
            }
            let next_start = end + 1;
            let next_len = self.line_end(next_start) - next_start;
            self.cursor = next_start + col.min(next_len);
        }
    }

    pub fn open_share(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.current_vault.clone() {
            self.share_code = self
                .backend
                .set_allow_connections(&id, true, None)
                .ok()
                .flatten();
            self.share_open = true;
            cx.notify();
        }
    }

    pub fn close_share(&mut self, cx: &mut Context<Self>) {
        self.share_open = false;
        cx.notify();
    }

    pub fn refresh_vaults(&mut self) {
        self.vaults = self.backend.list_vaults();
    }

    pub fn toggle_theme(&mut self, cx: &mut Context<Self>) {
        self.theme = if self.theme.dark {
            Theme::light()
        } else {
            Theme::dark()
        };
        cx.notify();
    }

    fn word_count(&self) -> usize {
        self.content.split_whitespace().count()
    }

    /// Vault display name = last path component.
    pub fn vault_name(v: &VaultInfo) -> String {
        std::path::Path::new(&v.path)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| v.path.clone())
    }

    pub fn open_vault(&mut self, id: &str, cx: &mut Context<Self>) {
        self.current_vault = Some(id.to_string());
        self.files = self.backend.list_files(id).unwrap_or_default();
        self.history = self.backend.history(id).unwrap_or_default();
        self.playhead_ts = None;
        self.screen = Screen::Editor;
        // Select the first markdown-ish file, else the first file.
        let first = self
            .files
            .iter()
            .find(|f| !f.is_dir && f.path.ends_with(".md"))
            .or_else(|| self.files.iter().find(|f| !f.is_dir))
            .map(|f| f.path.clone());
        if let Some(p) = first {
            self.select_file(&p, cx);
        } else {
            self.current_path = None;
            self.content.clear();
        }
        cx.notify();
    }

    pub fn select_file(&mut self, path: &str, cx: &mut Context<Self>) {
        if let Some(id) = self.current_vault.clone() {
            self.current_path = Some(path.to_string());
            self.editing = false;
            self.cursor = 0;
            self.reload_content(&id);
            cx.notify();
        }
    }

    /// Load the current file's content, honoring the time-travel playhead.
    fn reload_content(&mut self, id: &str) {
        let Some(path) = self.current_path.clone() else {
            self.content.clear();
            return;
        };
        self.content = match self.playhead_ts {
            Some(ts) => self
                .backend
                .read_file_at(id, &path, ts)
                .map(|f| if f.exists { f.content } else { String::new() })
                .unwrap_or_default(),
            None => self.backend.read_file(id, &path).unwrap_or_default(),
        };
    }

    /// Enter time-travel: view the vault as of `ts` (read-only).
    pub fn set_playhead(&mut self, ts: i64, cx: &mut Context<Self>) {
        if let Some(id) = self.current_vault.clone() {
            self.playhead_ts = Some(ts);
            self.editing = false;
            self.reload_content(&id);
            cx.notify();
        }
    }

    pub fn return_to_now(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.current_vault.clone() {
            self.playhead_ts = None;
            self.reload_content(&id);
            cx.notify();
        }
    }

    /// Restore the currently-viewed past version of the open file, then return
    /// to now. Forwards to `restore_file_at` (which records a new write).
    pub fn restore_version(&mut self, cx: &mut Context<Self>) {
        if let (Some(id), Some(path), Some(ts)) = (
            self.current_vault.clone(),
            self.current_path.clone(),
            self.playhead_ts,
        ) {
            let _ = self.backend.restore_file_at(&id, &path, ts);
            self.history = self.backend.history(&id).unwrap_or_default();
            self.playhead_ts = None;
            self.reload_content(&id);
            cx.notify();
        }
    }

    pub fn back_to_connect(&mut self, cx: &mut Context<Self>) {
        self.refresh_vaults();
        self.screen = Screen::Connect;
        cx.notify();
    }

    fn current_vault_info(&self) -> Option<&VaultInfo> {
        let id = self.current_vault.as_ref()?;
        self.vaults.iter().find(|v| &v.id == id)
    }
}

impl Render for AspApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match self.screen {
            Screen::Connect => self.render_connect(window, cx).into_any_element(),
            Screen::Editor => self.render_editor(window, cx).into_any_element(),
        }
    }
}

impl AspApp {
    fn render_connect(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let vaults = self.vaults.clone();

        // Logo + wordmark row.
        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(9.))
            .mb(px(34.))
            .child(
                div()
                    .w(px(26.))
                    .h(px(26.))
                    .rounded(px(7.))
                    .bg(t.accent),
            )
            .child(
                div()
                    .flex_1()
                    .text_color(t.text)
                    .text_size(px(16.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("asp"),
            )
            .child(
                div()
                    .id("connect-theme-toggle")
                    .w(px(28.))
                    .h(px(26.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(7.))
                    .text_color(t.text3)
                    .text_size(px(14.))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _e, _w, cx| this.toggle_theme(cx)))
                    .child(if t.dark { "☀" } else { "☾" }),
            );

        let title = div()
            .text_color(t.text)
            .text_size(px(25.))
            .font_weight(FontWeight::SEMIBOLD)
            .mb(px(22.))
            .child("Your vaults");

        let buttons = div()
            .flex()
            .flex_row()
            .gap(px(10.))
            .mb(px(26.))
            .child(
                div()
                    .id("new-vault")
                    .flex_1()
                    .h(px(46.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(8.))
                    .rounded(px(11.))
                    .bg(t.text)
                    .text_color(t.bg)
                    .text_size(px(14.))
                    .font_weight(FontWeight::MEDIUM)
                    .cursor_pointer()
                    .child("New vault"),
            )
            .child(
                div()
                    .id("connect-vault")
                    .flex_1()
                    .h(px(46.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(8.))
                    .rounded(px(11.))
                    .bg(t.bg)
                    .border_1()
                    .border_color(t.line)
                    .text_color(t.text)
                    .text_size(px(14.))
                    .font_weight(FontWeight::MEDIUM)
                    .cursor_pointer()
                    .child("Connect vault"),
            );

        // Recent vaults card.
        let mut list = div()
            .flex()
            .flex_col()
            .border_1()
            .border_color(t.line)
            .rounded(px(14.))
            .bg(t.bg)
            .overflow_hidden();

        if vaults.is_empty() {
            list = list.child(
                div()
                    .p(px(18.))
                    .text_color(t.faint)
                    .text_size(px(13.))
                    .child("No vaults yet — create or connect one to get started."),
            );
        } else {
            for (i, v) in vaults.iter().enumerate() {
                let name = Self::vault_name(v);
                let path = v.path.clone();
                let id = v.id.clone();
                let mut row = div()
                    .id(SharedString::from(format!("vault-{i}")))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(13.))
                    .px(px(15.))
                    .py(px(13.))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _ev, _w, cx| {
                        this.open_vault(&id, cx);
                    }));
                if i > 0 {
                    row = row.border_t_1().border_color(t.line);
                }
                row = row
                    .child(
                        div()
                            .w(px(34.))
                            .h(px(34.))
                            .rounded(px(9.))
                            .bg(t.accent_soft())
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(t.accent)
                            .text_size(px(15.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(first_letter(&name)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .child(
                                div()
                                    .text_color(t.text)
                                    .text_size(px(14.5))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(name),
                            )
                            .child(
                                div()
                                    .text_color(t.faint)
                                    .text_size(px(11.))
                                    .child(path),
                            ),
                    );
                list = list.child(row);
            }
        }

        let recent_label = div()
            .text_color(t.faint2)
            .text_size(px(11.))
            .font_weight(FontWeight::SEMIBOLD)
            .ml(px(3.))
            .mb(px(8.))
            .child("RECENT VAULTS");

        let card = div()
            .w(px(452.))
            .flex()
            .flex_col()
            .child(header)
            .child(title)
            .child(buttons)
            .child(recent_label)
            .child(list)
            .child(
                div()
                    .mt(px(20.))
                    .text_color(t.faint2)
                    .text_size(px(11.5))
                    .child(format!("This device · {}", short_fp(&self.backend.identity_ssh()))),
            );

        div()
            .size_full()
            .bg(t.bg_sub)
            .flex()
            .items_center()
            .justify_center()
            .text_color(t.text)
            .child(card)
    }

    fn render_editor(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let vault_name = self
            .current_vault_info()
            .map(Self::vault_name)
            .unwrap_or_else(|| "Vault".into());

        // ---- Sidebar ----
        let switcher = div()
            .id("vault-switcher")
            .h(px(47.))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.))
            .px(px(12.))
            .border_b_1()
            .border_color(t.line)
            .cursor_pointer()
            .on_click(cx.listener(|this, _ev, _w, cx| this.back_to_connect(cx)))
            .child(
                div()
                    .w(px(28.))
                    .h(px(28.))
                    .rounded(px(8.))
                    .bg(t.accent_soft())
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(t.accent)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(first_letter(&vault_name)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .child(
                        div()
                            .text_color(t.text)
                            .text_size(px(14.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(vault_name.clone()),
                    )
                    .child(
                        div()
                            .text_color(t.faint)
                            .text_size(px(11.))
                            .child("Synced"),
                    ),
            );

        let files_header = div()
            .flex()
            .flex_row()
            .items_center()
            .px(px(12.))
            .pt(px(9.))
            .pb(px(7.))
            .child(
                div()
                    .flex_1()
                    .text_color(t.faint2)
                    .text_size(px(11.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("FILES"),
            );

        // Virtualized file list: only the rows in view are built, so render cost
        // is O(visible) not O(file-count). The builder runs via `cx.processor`
        // so it executes inside this view's element/dispatch context — that is
        // what lets each row's `on_click` listener actually receive events (a
        // plain closure silently orphans them).
        let file_count = self.files.iter().filter(|f| !f.is_dir).count();
        let tree = gpui::uniform_list(
            "file-tree",
            file_count,
            cx.processor(|this, range: std::ops::Range<usize>, _window, cx| {
                let t = this.theme;
                let paths: Vec<String> = this
                    .files
                    .iter()
                    .filter(|f| !f.is_dir)
                    .map(|f| f.path.clone())
                    .collect();
                range
                    .filter_map(|i| paths.get(i).cloned().map(|p| (i, p)))
                    .map(|(i, path)| {
                        let is_active = this.current_path.as_deref() == Some(path.as_str());
                        let click_path = path.clone();
                        div()
                            .id(SharedString::from(format!("file-{i}")))
                            .h(px(29.))
                            // Full width so the whole row is the click target —
                            // a `uniform_list` item otherwise shrinks to its
                            // content width, leaving most of the row dead.
                            .w_full()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(6.))
                            .px(px(7.))
                            .rounded(px(5.))
                            .when(is_active, |d| d.bg(t.accent_soft()))
                            .text_color(if is_active { t.text } else { t.text2 })
                            .text_size(px(13.5))
                            .when(is_active, |d| d.font_weight(FontWeight::MEDIUM))
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _ev, _w, cx| {
                                this.select_file(&click_path, cx)
                            }))
                            .child(SharedString::from(path))
                    })
                    .collect()
            }),
        )
        .flex_1()
        .px(px(8.))
        .track_scroll(&self.file_scroll);

        let sidebar = div()
            .w(px(266.))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .bg(t.bg_sub)
            .border_r_1()
            .border_color(t.line)
            .child(switcher)
            .child(files_header)
            .child(tree);

        // ---- Main editor area ----
        let tab_label = self.current_path.clone().unwrap_or_else(|| "—".into());
        let header = div()
            .h(px(48.))
            .flex()
            .flex_row()
            .items_center()
            .px(px(16.))
            .gap(px(10.))
            .border_b_1()
            .border_color(t.line)
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_row()
                    .items_center()
                    .h_full()
                    .when(self.current_path.is_some(), |d| {
                        d.child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(7.))
                                .h_full()
                                .px(px(12.))
                                .border_t_2()
                                .border_color(t.accent)
                                .text_color(t.text)
                                .text_size(px(12.5))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(tab_label)
                                .child(
                                    div()
                                        .text_color(t.faint)
                                        .text_size(px(14.))
                                        .child("×"),
                                ),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.))
                    .child(div().w(px(7.)).h(px(7.)).rounded(px(4.)).bg(rgb(0x3fa45a)))
                    .child(
                        div()
                            .text_color(t.faint)
                            .text_size(px(12.))
                            .child(if self.saving { "Saving…" } else { "Saved" }),
                    )
                    .child(div().w(px(1.)).h(px(16.)).bg(t.line))
                    .child(
                        div()
                            .text_color(t.faint2)
                            .text_size(px(12.))
                            .child(format!("{} words", self.word_count())),
                    )
                    .child(div().w(px(1.)).h(px(16.)).bg(t.line))
                    .child(
                        div()
                            .id("share-btn")
                            .px(px(10.))
                            .py(px(5.))
                            .rounded(px(7.))
                            .bg(t.accent)
                            .text_color(t.bg)
                            .text_size(px(12.))
                            .font_weight(FontWeight::MEDIUM)
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _e, _w, cx| this.open_share(cx)))
                            .child("Share"),
                    )
                    .child(
                        div()
                            .id("theme-toggle")
                            .w(px(28.))
                            .h(px(26.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(7.))
                            .text_color(t.text3)
                            .text_size(px(14.))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _e, _w, cx| this.toggle_theme(cx)))
                            .child(if t.dark { "☀" } else { "☾" }),
                    ),
            );

        let body_inner: gpui::AnyElement = if let Some(path) = self.current_path.clone() {
            if self.editing {
                self.render_source_edit(&t)
            } else {
                let rendered = if path.ends_with(".md") {
                    crate::markdown::render_markdown(&self.content, &t)
                } else {
                    crate::markdown::render_code(&self.content, &t)
                };
                div()
                    .w(px(760.))
                    .px(px(40.))
                    .py(px(44.))
                    .child(rendered)
                    .into_any_element()
            }
        } else {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_color(t.faint2)
                .text_size(px(14.))
                .child("Select a note to start editing")
                .into_any_element()
        };

        let editable = self.current_path.is_some() && self.playhead_ts.is_none();
        let body = div()
            .id("editor-body")
            .track_focus(&self.focus)
            .flex_1()
            .flex()
            .justify_center()
            .overflow_hidden()
            .when(editable, |d| {
                d.cursor_pointer()
                    .on_click(cx.listener(|this, _e, window, cx| this.enter_edit(window, cx)))
                    .on_key_down(cx.listener(|this, ev, window, cx| this.on_key(ev, window, cx)))
            })
            .child(body_inner);

        let main = div()
            .flex_1()
            .flex()
            .flex_col()
            .bg(t.bg)
            .child(header)
            .when_some(self.time_travel_banner(cx), |d, banner| d.child(banner))
            .child(body);

        let top = div()
            .flex_1()
            .flex()
            .flex_row()
            .overflow_hidden()
            .child(sidebar)
            .child(main);

        div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(t.bg)
            .text_color(t.text)
            .child(top)
            .child(self.render_history_bar(cx))
            .when(self.share_open, |d| d.child(self.render_share_modal(cx)))
    }

    /// Render the raw Markdown source as one row per line (the original's 1:1
    /// line↔div invariant), drawing a caret on the cursor's line/column.
    fn render_source_edit(&self, t: &Theme) -> gpui::AnyElement {
        let cursor_line = self.content[..self.cursor].matches('\n').count();
        let line_start = self.line_start(self.cursor);
        let col_bytes = self.cursor - line_start;

        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        for (i, line) in self.content.split('\n').enumerate() {
            let row = if i == cursor_line {
                let (before, after) = line.split_at(col_bytes.min(line.len()));
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .min_h(px(22.))
                    .child(before.to_string())
                    .child(
                        div()
                            .w(px(2.))
                            .h(px(18.))
                            .bg(t.accent),
                    )
                    .child(after.to_string())
            } else {
                div().min_h(px(22.)).child(line.to_string())
            };
            rows.push(row.into_any_element());
        }

        div()
            .w(px(760.))
            .px(px(40.))
            .py(px(44.))
            .font_family("JetBrains Mono")
            .text_size(px(13.5))
            .line_height(px(22.))
            .text_color(t.text)
            .flex()
            .flex_col()
            .children(rows)
            .into_any_element()
    }

    fn render_share_modal(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = self.theme;
        let code = self
            .share_code
            .clone()
            .unwrap_or_else(|| "(no code available)".into());

        let panel = div()
            .w(px(420.))
            .flex()
            .flex_col()
            .gap(px(15.))
            .p(px(20.))
            .bg(t.bg)
            .rounded(px(16.))
            .border_1()
            .border_color(t.line)
            .child(
                div()
                    .text_color(t.text)
                    .text_size(px(16.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Share this vault"),
            )
            .child(
                div()
                    .text_color(t.text3)
                    .text_size(px(13.))
                    .child("Anyone you give this code to can connect and sync."),
            )
            .child(
                div()
                    .font_family("JetBrains Mono")
                    .text_size(px(12.))
                    .line_height(px(18.))
                    .text_color(t.text2)
                    .bg(t.bg_input)
                    .border_1()
                    .border_color(t.line)
                    .rounded(px(10.))
                    .p(px(13.))
                    .child(code),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .child(
                        div()
                            .id("share-done")
                            .px(px(18.))
                            .py(px(8.))
                            .rounded(px(9.))
                            .bg(t.text)
                            .text_color(t.bg)
                            .text_size(px(13.))
                            .font_weight(FontWeight::MEDIUM)
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _e, _w, cx| this.close_share(cx)))
                            .child("Done"),
                    ),
            );

        div()
            .absolute()
            .top(px(0.))
            .left(px(0.))
            .right(px(0.))
            .bottom(px(0.))
            .flex()
            .items_center()
            .justify_center()
            .bg(t.overlay)
            .child(panel)
            .into_any_element()
    }

    /// The accent banner shown above the editor while time-traveling.
    fn time_travel_banner(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let t = self.theme;
        let ts = self.playhead_ts?;
        Some(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(12.))
                .px(px(18.))
                .py(px(9.))
                .bg(t.accent_soft())
                .border_b_1()
                .border_color(t.accent)
                .child(
                    div()
                        .flex_1()
                        .text_color(t.text2)
                        .text_size(px(12.5))
                        .child(format!("Viewing this vault as it was on {} · read-only", fmt_ts(ts))),
                )
                .child(
                    div()
                        .id("restore-version")
                        .px(px(12.))
                        .py(px(6.))
                        .rounded(px(7.))
                        .bg(t.accent)
                        .text_color(t.bg)
                        .text_size(px(12.))
                        .font_weight(FontWeight::MEDIUM)
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _e, _w, cx| this.restore_version(cx)))
                        .child("Restore this version"),
                )
                .child(
                    div()
                        .id("return-now")
                        .px(px(12.))
                        .py(px(6.))
                        .rounded(px(7.))
                        .bg(t.bg)
                        .border_1()
                        .border_color(t.line)
                        .text_color(t.text2)
                        .text_size(px(12.))
                        .font_weight(FontWeight::MEDIUM)
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _e, _w, cx| this.return_to_now(cx)))
                        .child("Return to now"),
                )
                .into_any_element(),
        )
    }

    fn render_history_bar(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = self.theme;
        let path = self
            .current_vault_info()
            .map(|v| v.path.clone())
            .unwrap_or_default();
        let fp = short_fp(&self.backend.identity_ssh());

        // Status row: location · fingerprint · spacer · History/Log switcher.
        let status = div()
            .h(px(38.))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.))
            .pl(px(15.))
            .pr(px(9.))
            .child(
                div()
                    .font_family("JetBrains Mono")
                    .text_size(px(12.))
                    .text_color(t.text2)
                    .child(path),
            )
            .child(
                div()
                    .flex_1()
                    .font_family("JetBrains Mono")
                    .text_size(px(10.5))
                    .text_color(t.faint2)
                    .child(fp),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(2.))
                    .p(px(2.))
                    .rounded(px(8.))
                    .bg(t.line)
                    .child(
                        div()
                            .px(px(11.))
                            .py(px(4.))
                            .rounded(px(6.))
                            .bg(t.bg)
                            .text_size(px(12.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(t.text)
                            .child("History"),
                    )
                    .child(
                        div()
                            .px(px(11.))
                            .py(px(4.))
                            .rounded(px(6.))
                            .text_size(px(12.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(t.text3)
                            .child("Log"),
                    ),
            );

        // Track geometry.
        let (min_ts, max_ts) = self
            .history
            .iter()
            .fold((i64::MAX, i64::MIN), |(lo, hi), e| (lo.min(e.ts), hi.max(e.ts)));
        let span = if max_ts > min_ts { (max_ts - min_ts) as f64 } else { 1.0 };
        let frac_of = |ts: i64| -> f32 {
            if max_ts <= min_ts {
                0.5
            } else {
                ((ts - min_ts) as f64 / span) as f32
            }
        };
        let play_frac = self.playhead_ts.map(frac_of).unwrap_or(1.0);

        let mut track = div()
            .relative()
            .flex_1()
            .mx(px(16.))
            .mb(px(11.))
            // center line
            .child(
                div()
                    .absolute()
                    .top(relative(0.5))
                    .left(px(0.))
                    .right(px(0.))
                    .h(px(1.))
                    .bg(t.line),
            );

        // Cap rendered ticks regardless of event count: a vault with thousands
        // of history events must not paint thousands of dots every frame (that
        // dominated editor frame time). Sample evenly to ~MAX_TICKS, always
        // keeping the last event.
        const MAX_TICKS: usize = 240;
        let n = self.history.len();
        let step = (n / MAX_TICKS).max(1);
        for (i, e) in self.history.iter().enumerate() {
            if step > 1 && i % step != 0 && i + 1 != n {
                continue;
            }
            let frac = frac_of(e.ts);
            let color = kind_color(&e.kind, &t);
            let ts = e.ts;
            let past = self.playhead_ts.map(|p| ts <= p).unwrap_or(true);
            track = track.child(
                div()
                    .id(SharedString::from(format!("hist-{i}")))
                    .absolute()
                    .left(relative(frac))
                    .top(relative(0.5))
                    .mt(px(-7.))
                    .ml(px(-7.))
                    .w(px(14.))
                    .h(px(14.))
                    .rounded(px(7.))
                    .border_2()
                    .border_color(color)
                    .when(past, |d| d.bg(color))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _e, _w, cx| this.set_playhead(ts, cx))),
            );
        }

        // Playhead marker.
        track = track.child(
            div()
                .absolute()
                .left(relative(play_frac))
                .top(px(2.))
                .bottom(px(2.))
                .ml(px(-1.))
                .w(px(2.))
                .bg(t.accent),
        );

        let toolbar = div()
            .flex()
            .flex_row()
            .items_center()
            .px(px(16.))
            .pt(px(6.))
            .pb(px(2.))
            .child(
                div()
                    .flex_1()
                    .text_size(px(11.))
                    .text_color(t.faint2)
                    .child(format!("{} events", self.history.len())),
            )
            .child(
                div()
                    .id("hist-now")
                    .px(px(12.))
                    .py(px(4.))
                    .rounded(px(7.))
                    .bg(t.bg)
                    .border_1()
                    .border_color(t.line)
                    .text_size(px(12.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(if self.playhead_ts.is_some() { t.text2 } else { t.faint2 })
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _e, _w, cx| this.return_to_now(cx)))
                    .child("Now"),
            );

        div()
            .h(px(150.))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .bg(t.bg_sub)
            .border_t_1()
            .border_color(t.line)
            .child(status)
            .child(toolbar)
            .child(track)
            .into_any_element()
    }
}

fn kind_color(kind: &str, _t: &Theme) -> Rgba {
    match kind {
        "create" => rgb(0x3fa45a),
        "edit" => rgb(0x3d63dd),
        "rename" => rgb(0xd9a93d),
        _ => rgb(0xd96a6a),
    }
}

/// Format a unix-seconds timestamp as `YYYY-MM-DD HH:MM` (UTC), no chrono dep.
fn fmt_ts(ts: i64) -> String {
    let days = ts.div_euclid(86400);
    let secs = ts.rem_euclid(86400);
    let (h, m) = (secs / 3600, (secs % 3600) / 60);
    // Howard Hinnant's civil_from_days.
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mth = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mth <= 2 { y + 1 } else { y };
    format!("{y:04}-{mth:02}-{d:02} {h:02}:{m:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_ts_known_instants() {
        // Unix epoch.
        assert_eq!(fmt_ts(0), "1970-01-01 00:00");
        // 2021-01-01 00:00:00 UTC = 1609459200.
        assert_eq!(fmt_ts(1609459200), "2021-01-01 00:00");
        // 2026-06-27 12:34:00 UTC = 1782563640.
        assert_eq!(fmt_ts(1782563640), "2026-06-27 12:34");
    }

    #[test]
    fn short_fp_takes_key_tail() {
        let ssh = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAITAILEND comment";
        assert_eq!(short_fp(ssh), "ITAILEND");
    }

    #[test]
    fn kind_colors_distinct() {
        let t = Theme::light();
        assert_ne!(kind_color("create", &t), kind_color("edit", &t));
        assert_ne!(kind_color("rename", &t), kind_color("delete", &t));
    }
}

fn first_letter(s: &str) -> String {
    s.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default()
}

fn short_fp(ssh: &str) -> String {
    // ssh-ed25519 AAAA... comment → show a short tail of the key body.
    let body = ssh.split_whitespace().nth(1).unwrap_or(ssh);
    let tail: String = body.chars().rev().take(8).collect::<String>().chars().rev().collect();
    tail
}
