//! `AspApp` — the stateful root entity. Owns the engine handle and all UI state
//! (current screen, open vault, file tree, tabs, active file + content), and
//! dispatches rendering to the data-driven `screens::{connect,editor}` modules.
//! Interactivity (clicks) mutates this state and calls `cx.notify()`.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::{div, prelude::*, AnyElement};

use crate::engine::Engine;
use crate::screens::{connect, editor};
use crate::theme::{self, Theme};
use crate::vault::format::short_fingerprint;
use crate::vault::tabs;
use crate::vault::tree;
use crate::vault::vault_meta::{self, VaultMetaMap};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Screen {
    Connect,
    Editor,
}

/// View-model for one Connect-screen vault row.
#[derive(Clone)]
pub struct ConnectRow {
    pub id: String,       // engine session id (for opening)
    pub vault_id: String, // stable cross-session id (meta key)
    pub name: String,
    pub hue: f32,
    pub emoji: Option<String>,
    pub location: String,
    pub time: String,
    pub loading: bool,
    pub is_web: bool,
}

pub struct AspApp {
    pub engine: Option<Rc<Engine>>,
    pub theme: Theme,
    pub fingerprint: String,
    pub screen: Screen,
    pub is_web: bool,

    // Connect screen.
    pub connect_rows: Vec<ConnectRow>,
    /// Persisted per-vault cosmetic metadata, keyed by stable `vault_id`.
    pub meta: VaultMetaMap,

    // Editor screen.
    pub vault_id: Option<String>,
    pub vault_name: String,
    pub vault_hue: f32,
    pub peers: usize,
    pub files: Vec<(String, bool)>, // (path, is_dir)
    pub expanded: HashMap<String, bool>,
    pub tabs: Vec<String>,
    pub active: Option<String>,
    pub content: String,

    // History / time-travel.
    pub history_events: Vec<crate::vault::history::TrackEvent>,
    /// When `Some(unix_secs)`, the editor shows the vault as-of that time (read-only).
    pub playhead: Option<i64>,

    // Transient overlays.
    pub menu: Menu,
    pub modal: Modal,

    // Editing.
    pub editing: bool,
    pub buffer: crate::vault::textbuffer::TextBuffer,
    pub focus: Option<gpui::FocusHandle>,

    // Layout (resizable).
    pub sidebar_w: f32,
    pub dragging_sidebar: bool,
    pub hist_bar_h: f32,
    pub dragging_hist: bool,

    /// Whether the background live-sync poll loop has been started.
    pub polling_started: bool,
}

/// An open context menu (anchored at a click position).
#[derive(Clone, PartialEq, Debug)]
pub enum Menu {
    None,
    Vault { id: String, name: String, x: f32, y: f32 },
    Tab { path: String, x: f32, y: f32 },
    File { path: String, is_dir: bool, x: f32, y: f32 },
}

/// An open modal dialog.
#[derive(Clone, PartialEq, Debug)]
pub enum Modal {
    None,
    RemoveVault { id: String, name: String, trash: bool },
    ShareVault { name: String, ticket: Option<String> },
    ConnectVault { buf: crate::vault::textbuffer::TextBuffer },
    Customize {
        vault_id: String,
        buf: crate::vault::textbuffer::TextBuffer,
        hue: f32,
        emoji: Option<String>,
    },
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl AspApp {
    /// The live app: real engine, real vaults.
    pub fn new() -> Self {
        let (engine, fingerprint) = match Engine::new() {
            Ok(e) => {
                let fp = short_fingerprint(&e.identity_ssh());
                (Some(Rc::new(e)), fp)
            }
            Err(e) => {
                log::error!("failed to init engine: {e}");
                (None, "unknown".to_string())
            }
        };
        let mut app = AspApp {
            engine,
            theme: Theme::light(),
            fingerprint,
            screen: Screen::Connect,
            is_web: false,
            connect_rows: Vec::new(),
            meta: vault_meta::load_meta(),
            vault_id: None,
            vault_name: String::new(),
            vault_hue: theme::VAULT_HUES[0],
            peers: 0,
            files: Vec::new(),
            expanded: HashMap::new(),
            tabs: Vec::new(),
            active: None,
            content: String::new(),
            history_events: Vec::new(),
            playhead: None,
            menu: Menu::None,
            modal: Modal::None,
            editing: false,
            buffer: crate::vault::textbuffer::TextBuffer::default(),
            focus: None,
            sidebar_w: 266.0,
            dragging_sidebar: false,
            hist_bar_h: 150.0,
            dragging_hist: false,
            polling_started: false,
        };
        // Apply persisted UI prefs (theme + panel sizes).
        let prefs = crate::vault::prefs::load_prefs();
        app.theme = if prefs.theme == theme::Appearance::Dark {
            Theme::dark()
        } else {
            Theme::light()
        };
        app.sidebar_w = prefs.sidebar_w;
        app.hist_bar_h = prefs.hist_bar_h;
        app.refresh_vaults();
        app
    }

    /// Persist UI prefs (theme + panel sizes). Live app only — fixtures/tests
    /// (engine = None) must not write to `~/.asp`.
    fn save_prefs_now(&self) {
        if self.engine.is_none() {
            return;
        }
        let prefs = crate::vault::prefs::Prefs {
            theme: self.theme.appearance,
            sidebar_w: self.sidebar_w,
            hist_bar_h: self.hist_bar_h,
            ..Default::default()
        };
        crate::vault::prefs::save_prefs(&prefs);
    }

    /// Fixture Connect screen (no engine) — for screenshots / visual checks.
    pub fn fixture_connect(theme: Theme) -> Self {
        let rows = vec![
            ConnectRow {
                id: "v1".into(),
                vault_id: "v1".into(),
                name: "Research Notes".into(),
                hue: theme::VAULT_HUES[0],
                emoji: None,
                location: "~/vaults/research".into(),
                time: "2h ago".into(),
                loading: false,
                is_web: false,
            },
            ConnectRow {
                id: "v2".into(),
                vault_id: "v2".into(),
                name: "Journal".into(),
                hue: theme::VAULT_HUES[3],
                emoji: Some("📔".into()),
                location: "~/Documents/journal".into(),
                time: "yesterday".into(),
                loading: false,
                is_web: false,
            },
            ConnectRow {
                id: "v3".into(),
                vault_id: "v3".into(),
                name: "Shared Wiki".into(),
                hue: theme::VAULT_HUES[1],
                emoji: None,
                location: "Opening…".into(),
                time: "Opening…".into(),
                loading: true,
                is_web: false,
            },
        ];
        let mut app = Self::fixture_base(theme);
        app.connect_rows = rows;
        app
    }

    /// Fixture Editor screen (no engine) — for screenshots / visual checks.
    pub fn fixture_editor(theme: Theme) -> Self {
        let mut app = Self::fixture_base(theme);
        app.screen = Screen::Editor;
        app.vault_name = "Research Notes".into();
        app.vault_hue = theme::VAULT_HUES[0];
        app.peers = 2;
        app.files = vec![
            ("README.md".into(), false),
            ("drafts".into(), true),
            ("drafts/launch-post.md".into(), false),
            ("notes".into(), true),
            ("notes/ideas.md".into(), false),
            ("notes/todo.md".into(), false),
            ("changelog.md".into(), false),
        ];
        app.expanded.insert("notes".into(), true);
        app.expanded.insert("drafts".into(), true);
        app.tabs = vec!["README.md".into(), "notes/ideas.md".into(), "notes/todo.md".into()];
        app.active = Some("README.md".into());
        app.content = "# Research Notes\n\nA **local-first** vault for research. Everything syncs *peer-to-peer* with end-to-end encryption — no servers in between.\n\n## Principles\n\n- The source is preserved `byte-for-byte` underneath\n- Edits broadcast to connected peers in real time\n- Every change is in the [history](#history) log\n\n## Today\n\n- [x] Wire the engine to the editor\n- [x] Render markdown live\n- [ ] Port the history scrubber\n\n> Use the scrubber below to travel back through every edit.\n\n```rust\nfn main() {\n    println!(\"hello, vault\");\n}\n```".into();
        app
    }

    fn fixture_base(theme: Theme) -> Self {
        AspApp {
            engine: None,
            theme,
            fingerprint: "a1b2c3d4".into(),
            screen: Screen::Connect,
            is_web: false,
            connect_rows: Vec::new(),
            meta: HashMap::new(),
            vault_id: None,
            vault_name: String::new(),
            vault_hue: theme::VAULT_HUES[0],
            peers: 0,
            files: Vec::new(),
            expanded: HashMap::new(),
            tabs: Vec::new(),
            active: None,
            content: String::new(),
            history_events: Vec::new(),
            playhead: None,
            menu: Menu::None,
            modal: Modal::None,
            editing: false,
            buffer: crate::vault::textbuffer::TextBuffer::default(),
            focus: None,
            sidebar_w: 266.0,
            dragging_sidebar: false,
            hist_bar_h: 150.0,
            dragging_hist: false,
            polling_started: false,
        }
    }

    /// Rebuild the Connect rows from live engine data.
    pub fn refresh_vaults(&mut self) {
        let Some(eng) = self.engine.clone() else { return };
        let now = now_secs();
        let mut rows = Vec::new();
        for v in eng.list_vaults() {
            let status = eng.status(&v.id).ok();
            let last_ts = status.as_ref().and_then(|s| s.last_ts);
            let base = std::path::Path::new(&v.path)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(&v.path)
                .to_string();
            let m = vault_meta::resolve_meta(&self.meta, &v.vault_id, &base);
            rows.push(ConnectRow {
                id: v.id.clone(),
                vault_id: v.vault_id.clone(),
                name: m.name,
                hue: m.hue as f32,
                emoji: m.emoji,
                location: v.path.clone(),
                time: crate::vault::format::rel_time(last_ts, now),
                loading: false,
                is_web: false,
            });
        }
        self.connect_rows = rows;
    }

    /// Add a local folder as a vault and open it (from the native picker).
    pub fn add_local_folder_path(&mut self, path: &std::path::Path) {
        if let Some(eng) = self.engine.clone() {
            if let Ok(info) = eng.add_local_folder(path) {
                self.refresh_vaults();
                self.open_vault(&info.id);
            }
        }
    }

    /// Open a managed vault by engine session id → switch to the editor.
    pub fn open_vault(&mut self, id: &str) {
        if let Some(eng) = self.engine.clone() {
            if let Ok(files) = eng.list_files(id) {
                self.files = files.iter().map(|f| (f.path.clone(), f.is_dir)).collect();
            }
            let nodes = tree::build_tree(self.files.iter().map(|(p, d)| (p.as_str(), *d)));
            self.expanded = tree::all_dir_paths(&nodes).into_iter().map(|p| (p, true)).collect();
            self.active = tree::first_selectable(&nodes);
            self.content = self
                .active
                .as_ref()
                .and_then(|p| eng.read_file(id, p).ok())
                .unwrap_or_default();
            self.tabs = self.active.clone().into_iter().collect();
            // vault display
            if let Some(row) = self.connect_rows.iter().find(|r| r.id == id) {
                self.vault_name = row.name.clone();
                self.vault_hue = row.hue;
            }
            if let Ok(st) = eng.status(id) {
                self.peers = st.peers.len();
            }
        }
        self.vault_id = Some(id.to_string());
        self.playhead = None;
        self.editing = false;
        self.load_history(id);
        self.screen = Screen::Editor;
    }

    /// Load the vault's history into track events (unix-seconds → ms model).
    fn load_history(&mut self, id: &str) {
        self.history_events.clear();
        if let Some(eng) = self.engine.clone() {
            if let Ok(hist) = eng.history(id) {
                let tuples: Vec<(String, i64, String, String)> = hist
                    .into_iter()
                    .map(|e| (e.id, e.ts, e.kind, e.path))
                    .collect();
                self.history_events = crate::vault::history::build_events(&tuples);
            }
        }
    }

    /// Re-read the active file's LIVE content (exits any time-travel view).
    fn reload_active(&mut self) {
        if let (Some(eng), Some(vid), Some(p)) =
            (self.engine.clone(), self.vault_id.clone(), self.active.clone())
        {
            self.content = eng.read_file(&vid, &p).unwrap_or_default();
        }
    }

    /// True when viewing a past version (read-only).
    pub fn is_time_travel(&self) -> bool {
        self.playhead.is_some()
    }

    /// View the active file as-of `ts_secs` (read-only time-travel).
    pub fn time_travel_to(&mut self, ts_secs: i64) {
        self.playhead = Some(ts_secs);
        self.editing = false;
        if let (Some(eng), Some(vid), Some(p)) =
            (self.engine.clone(), self.vault_id.clone(), self.active.clone())
        {
            match eng.read_file_at(&vid, &p, ts_secs) {
                Ok(at) if at.exists => self.content = at.content,
                _ => self.content.clear(),
            }
        }
    }

    /// Exit time-travel, returning to the live (now) version.
    pub fn return_to_now(&mut self) {
        self.playhead = None;
        self.reload_active();
    }

    /// Restore the active file to the currently-viewed past version.
    pub fn restore_version(&mut self) {
        if let (Some(eng), Some(vid), Some(p), Some(ts)) = (
            self.engine.clone(),
            self.vault_id.clone(),
            self.active.clone(),
            self.playhead,
        ) {
            let _ = eng.restore_file_at(&vid, &p, ts);
            self.playhead = None;
            self.load_history(&vid);
            self.reload_active();
        }
    }

    /// Select a file in the tree → load its content + open a tab.
    pub fn select_file(&mut self, path: &str) {
        self.active = Some(path.to_string());
        self.editing = false;
        self.tabs = tabs::with_tab(&self.tabs, path);
        if let (Some(eng), Some(vid)) = (self.engine.clone(), self.vault_id.clone()) {
            self.content = eng.read_file(&vid, path).unwrap_or_default();
        }
    }

    /// Expand/collapse a folder.
    pub fn toggle_dir(&mut self, path: &str) {
        let e = self.expanded.entry(path.to_string()).or_insert(false);
        *e = !*e;
    }

    /// Close a tab, picking the right neighbor to activate.
    pub fn close_tab(&mut self, path: &str) {
        let res = tabs::close_tab(&self.tabs, self.active.as_deref(), path);
        self.tabs = res.tabs;
        if self.active.as_deref() != res.active.as_deref() {
            self.active = res.active;
            if let (Some(eng), Some(vid), Some(p)) =
                (self.engine.clone(), self.vault_id.clone(), self.active.clone())
            {
                self.content = eng.read_file(&vid, &p).unwrap_or_default();
            } else if self.active.is_none() {
                self.content.clear();
            }
        }
    }

    /// Toggle light/dark theme.
    pub fn toggle_theme(&mut self) {
        self.theme = match self.theme.appearance {
            crate::theme::Appearance::Light => Theme::dark(),
            crate::theme::Appearance::Dark => Theme::light(),
        };
        self.save_prefs_now();
    }

    /// Re-list the open vault's files (after a file operation).
    fn reload_files(&mut self) {
        if let (Some(eng), Some(vid)) = (self.engine.clone(), self.vault_id.clone()) {
            if let Ok(files) = eng.list_files(&vid) {
                self.files = files.iter().map(|f| (f.path.clone(), f.is_dir)).collect();
            }
        }
    }

    /// Create a fresh `untitled[-n].md` at the vault root and open it.
    pub fn new_file(&mut self) {
        let (Some(eng), Some(vid)) = (self.engine.clone(), self.vault_id.clone()) else { return };
        let siblings: std::collections::HashSet<String> = self
            .files
            .iter()
            .filter(|(p, _)| !p.contains('/'))
            .map(|(p, _)| p.clone())
            .collect();
        let name = crate::vault::format::free_name(&siblings, ".md");
        if eng.write_file(&vid, &name, "# Untitled\n\n").is_ok() {
            self.reload_files();
            self.select_file(&name);
        }
    }

    /// Delete a file and fix up tabs/selection.
    pub fn delete_file(&mut self, path: &str) {
        let (Some(eng), Some(vid)) = (self.engine.clone(), self.vault_id.clone()) else { return };
        if eng.delete_file(&vid, path).is_err() {
            return;
        }
        self.reload_files();
        // drop the tab (and its subtree) and re-point the active file if needed.
        let was_active = self.active.as_deref() == Some(path);
        self.tabs = tabs::remove_tabs(&self.tabs, &[path.to_string()]);
        if was_active {
            self.active = self.tabs.last().cloned();
            self.content = match (self.active.clone(), self.engine.clone(), self.vault_id.clone()) {
                (Some(p), Some(eng), Some(vid)) => eng.read_file(&vid, &p).unwrap_or_default(),
                _ => String::new(),
            };
        }
    }

    /// Rename a file/folder and remap tabs + active + expanded.
    #[allow(dead_code)]
    pub fn rename_file(&mut self, old: &str, new: &str) {
        let (Some(eng), Some(vid)) = (self.engine.clone(), self.vault_id.clone()) else { return };
        if eng.rename_file(&vid, old, new).is_err() {
            return;
        }
        self.reload_files();
        self.tabs = tabs::remap_tabs(&self.tabs, old, new);
        if self.active.as_deref() == Some(old) {
            self.active = Some(new.to_string());
        } else if let Some(a) = &self.active {
            if let Some(rest) = a.strip_prefix(&format!("{old}/")) {
                self.active = Some(format!("{new}/{rest}"));
            }
        }
    }

    /// Return to the Connect screen.
    pub fn back_to_connect(&mut self) {
        self.refresh_vaults();
        self.screen = Screen::Connect;
    }

    pub fn active_path(&self) -> Option<&str> {
        self.active.as_deref()
    }

    // -- context menu / modal --

    pub fn open_vault_menu(&mut self, id: &str, name: &str, x: f32, y: f32) {
        self.menu = Menu::Vault { id: id.to_string(), name: name.to_string(), x, y };
    }

    pub fn open_tab_menu(&mut self, path: &str, x: f32, y: f32) {
        self.menu = Menu::Tab { path: path.to_string(), x, y };
    }

    pub fn open_file_menu(&mut self, path: &str, is_dir: bool, x: f32, y: f32) {
        self.menu = Menu::File { path: path.to_string(), is_dir, x, y };
    }

    pub fn close_menu(&mut self) {
        self.menu = Menu::None;
    }

    /// Set the active file (loading its content) — no-op if already active.
    fn set_active(&mut self, path: Option<String>) {
        if self.active == path {
            return;
        }
        self.active = path;
        self.editing = false;
        match (&self.active, self.engine.clone(), self.vault_id.clone()) {
            (Some(p), Some(eng), Some(vid)) => {
                self.content = eng.read_file(&vid, p).unwrap_or_default()
            }
            _ => self.content.clear(),
        }
    }

    /// Ensure `active` is still an open tab; if not, pick the last one (or none).
    fn reconcile_active(&mut self) {
        let still_open = self
            .active
            .as_ref()
            .map(|a| self.tabs.iter().any(|t| t == a))
            .unwrap_or(false);
        if !still_open {
            self.set_active(self.tabs.last().cloned());
        }
    }

    pub fn close_others(&mut self, path: &str) {
        self.tabs = tabs::close_others(&self.tabs, path);
        self.set_active(Some(path.to_string()));
    }

    pub fn close_to_left(&mut self, path: &str) {
        self.tabs = tabs::close_to_left(&self.tabs, path);
        self.reconcile_active();
    }

    pub fn close_to_right(&mut self, path: &str) {
        self.tabs = tabs::close_to_right(&self.tabs, path);
        self.reconcile_active();
    }

    pub fn close_all_tabs(&mut self) {
        self.tabs.clear();
        self.set_active(None);
    }

    // -- layout resize --

    pub fn start_sidebar_drag(&mut self) {
        self.dragging_sidebar = true;
    }

    /// While dragging, set the sidebar width to the pointer x (clamped).
    pub fn drag_sidebar(&mut self, x: f32) {
        if self.dragging_sidebar {
            self.sidebar_w = crate::vault::prefs::clamp_sidebar(x);
        }
    }

    pub fn start_hist_drag(&mut self) {
        self.dragging_hist = true;
    }

    /// While dragging, set the history-bar height (clamped). `h` is the desired
    /// height (window-bottom minus pointer y, computed by the caller).
    pub fn drag_hist(&mut self, h: f32) {
        if self.dragging_hist {
            self.hist_bar_h = crate::vault::prefs::clamp_hist_bar(h);
        }
    }

    pub fn end_drag(&mut self) {
        let was_dragging = self.dragging_sidebar || self.dragging_hist;
        self.dragging_sidebar = false;
        self.dragging_hist = false;
        if was_dragging {
            self.save_prefs_now();
        }
    }

    /// Live-sync poll: pick up peer edits the engine has materialized. Skips while
    /// editing (don't clobber unsaved input) or time-travelling.
    pub fn poll_refresh(&mut self) {
        if self.screen != Screen::Editor || self.editing || self.is_time_travel() {
            return;
        }
        let (Some(eng), Some(vid)) = (self.engine.clone(), self.vault_id.clone()) else {
            return;
        };
        if let Ok(files) = eng.list_files(&vid) {
            self.files = files.iter().map(|f| (f.path.clone(), f.is_dir)).collect();
        }
        if let Some(p) = self.active.clone() {
            if let Ok(c) = eng.read_file(&vid, &p) {
                self.content = c;
            }
        }
        if let Ok(st) = eng.status(&vid) {
            self.peers = st.peers.len();
        }
        self.load_history(&vid);
    }

    /// Open the "share vault" modal — enables connections and shows the ticket.
    pub fn open_share(&mut self, id: &str, name: &str) {
        self.menu = Menu::None;
        let ticket = self
            .engine
            .clone()
            .and_then(|eng| eng.set_allow_connections(id, true, None).ok().flatten());
        self.modal = Modal::ShareVault { name: name.to_string(), ticket };
    }

    /// Open the "remove vault" confirmation modal (also closes any menu).
    pub fn open_remove(&mut self, id: &str, name: &str) {
        self.menu = Menu::None;
        self.modal = Modal::RemoveVault { id: id.to_string(), name: name.to_string(), trash: false };
    }

    pub fn close_modal(&mut self) {
        self.modal = Modal::None;
    }

    /// Open the "customize vault" modal (name + hue), seeded from the row.
    pub fn open_customize(&mut self, id: &str) {
        self.menu = Menu::None;
        if let Some(row) = self.connect_rows.iter().find(|r| r.id == id) {
            self.modal = Modal::Customize {
                vault_id: row.vault_id.clone(),
                buf: crate::vault::textbuffer::TextBuffer::new(row.name.clone()),
                hue: row.hue,
                emoji: row.emoji.clone(),
            };
        }
    }

    pub fn customize_type(&mut self, s: &str) {
        if let Modal::Customize { buf, .. } = &mut self.modal {
            buf.insert(s);
        }
    }
    pub fn customize_backspace(&mut self) {
        if let Modal::Customize { buf, .. } = &mut self.modal {
            buf.backspace();
        }
    }
    pub fn customize_set_hue(&mut self, h: f32) {
        if let Modal::Customize { hue, .. } = &mut self.modal {
            *hue = h;
        }
    }

    /// Persist the customization (name + hue) keyed by stable `vault_id`.
    pub fn customize_apply(&mut self) {
        if let Modal::Customize { vault_id, buf, hue, emoji } = self.modal.clone() {
            let name = buf.text.trim().to_string();
            self.meta.insert(
                vault_id,
                crate::vault::vault_meta::VaultMetaEntry {
                    name: if name.is_empty() { None } else { Some(name) },
                    hue: hue as f64,
                    emoji,
                },
            );
            crate::vault::vault_meta::save_meta(&self.meta);
            self.refresh_vaults();
        }
        self.modal = Modal::None;
    }

    /// Open the "connect vault" modal (paste a ticket).
    pub fn open_connect(&mut self) {
        self.menu = Menu::None;
        self.modal = Modal::ConnectVault { buf: crate::vault::textbuffer::TextBuffer::new("") };
    }

    /// Insert text into the connect-ticket field.
    pub fn connect_type(&mut self, s: &str) {
        if let Modal::ConnectVault { buf } = &mut self.modal {
            buf.insert(s);
        }
    }

    pub fn connect_backspace(&mut self) {
        if let Modal::ConnectVault { buf } = &mut self.modal {
            buf.backspace();
        }
    }

    pub fn connect_ticket(&self) -> Option<String> {
        match &self.modal {
            Modal::ConnectVault { buf } => Some(buf.text.clone()),
            _ => None,
        }
    }

    /// Clone the shared vault (from the typed ticket) into `dest` and open it.
    pub fn connect_confirm(&mut self, dest: &std::path::Path) {
        let Some(ticket) = self.connect_ticket() else { return };
        let ticket = ticket.trim().to_string();
        if ticket.is_empty() {
            return;
        }
        if let Some(eng) = self.engine.clone() {
            if let Ok(info) = eng.clone_remote(dest, &ticket, None) {
                self.modal = Modal::None;
                self.refresh_vaults();
                self.open_vault(&info.id);
                return;
            }
        }
        // on failure, leave the modal open so the user can fix the ticket.
    }

    /// Toggle the "move to trash" checkbox in the remove modal.
    pub fn toggle_remove_trash(&mut self) {
        if let Modal::RemoveVault { trash, .. } = &mut self.modal {
            *trash = !*trash;
        }
    }

    /// Confirm removal: stop managing the vault, refresh the list, close the modal.
    pub fn confirm_remove(&mut self) {
        if let Modal::RemoveVault { id, trash, .. } = self.modal.clone() {
            if let Some(eng) = self.engine.clone() {
                let _ = eng.remove_vault(&id, trash);
            }
            self.refresh_vaults();
        }
        self.modal = Modal::None;
    }

    // -- editing --

    /// Enter edit mode for the active file (no-op during time-travel).
    pub fn begin_edit(&mut self) {
        if self.is_time_travel() || self.active.is_none() {
            return;
        }
        self.buffer = crate::vault::textbuffer::TextBuffer::new(self.content.clone());
        self.editing = true;
    }

    pub fn stop_edit(&mut self) {
        self.editing = false;
    }

    /// Push the buffer into `content` and persist via the engine.
    fn after_edit(&mut self) {
        self.content = self.buffer.text.clone();
        if let (Some(eng), Some(vid), Some(p)) =
            (self.engine.clone(), self.vault_id.clone(), self.active.clone())
        {
            let _ = eng.write_file(&vid, &p, &self.content);
        }
    }

    pub fn type_text(&mut self, s: &str) {
        if !self.editing {
            return;
        }
        self.buffer.insert(s);
        self.after_edit();
    }

    pub fn editor_backspace(&mut self) {
        if !self.editing {
            return;
        }
        self.buffer.backspace();
        self.after_edit();
    }

    pub fn editor_delete(&mut self) {
        if !self.editing {
            return;
        }
        self.buffer.delete();
        self.after_edit();
    }

    pub fn editor_newline(&mut self) {
        if !self.editing {
            return;
        }
        self.buffer.insert("\n");
        self.after_edit();
    }

    pub fn editor_move(&mut self, dir: CaretMove) {
        if !self.editing {
            return;
        }
        match dir {
            CaretMove::Left => self.buffer.move_left(),
            CaretMove::Right => self.buffer.move_right(),
            CaretMove::Up => self.buffer.move_up(),
            CaretMove::Down => self.buffer.move_down(),
            CaretMove::Home => self.buffer.home(),
            CaretMove::End => self.buffer.end(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum CaretMove {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
}

impl Render for AspApp {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.focus.is_none() {
            self.focus = Some(cx.focus_handle());
        }
        // Start the live-sync poll loop once (live app only).
        if !self.polling_started && self.engine.is_some() {
            self.polling_started = true;
            let bg = cx.background_executor().clone();
            cx.spawn(async move |this, cx| loop {
                bg.timer(Duration::from_secs(2)).await;
                if this
                    .update(cx, |this, cx| {
                        this.poll_refresh();
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            })
            .detach();
        }
        let screen_el: AnyElement = match self.screen {
            Screen::Connect => connect::render(self, cx).into_any_element(),
            Screen::Editor => editor::render(self, cx).into_any_element(),
        };
        let mut root = div().relative().size_full().child(screen_el);
        if let Some(menu) = crate::screens::overlays::vault_menu(self, cx) {
            root = root.child(menu);
        }
        if let Some(menu) = crate::screens::overlays::tab_menu(self, cx) {
            root = root.child(menu);
        }
        if let Some(menu) = crate::screens::overlays::file_menu(self, cx) {
            root = root.child(menu);
        }
        if let Some(modal) = crate::screens::overlays::remove_modal(self, cx) {
            root = root.child(modal);
        }
        if let Some(modal) = crate::screens::overlays::share_modal(self, cx) {
            root = root.child(modal);
        }
        if let Some(modal) = crate::screens::overlays::connect_modal(self, cx) {
            root = root.child(modal);
        }
        if let Some(modal) = crate::screens::overlays::customize_modal(self, cx) {
            root = root.child(modal);
        }
        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use asp_core::Identity;

    #[test]
    fn fixture_editor_state_transitions() {
        let mut app = AspApp::fixture_editor(Theme::light());
        // toggle_dir flips expanded
        assert_eq!(app.expanded.get("notes"), Some(&true));
        app.toggle_dir("notes");
        assert_eq!(app.expanded.get("notes"), Some(&false));
        app.toggle_dir("notes");
        assert_eq!(app.expanded.get("notes"), Some(&true));
        // select_file updates active + opens a tab
        app.select_file("changelog.md");
        assert_eq!(app.active.as_deref(), Some("changelog.md"));
        assert!(app.tabs.iter().any(|t| t == "changelog.md"));
        // closing the active tab selects a neighbor
        app.close_tab("changelog.md");
        assert!(!app.tabs.iter().any(|t| t == "changelog.md"));
        assert_ne!(app.active.as_deref(), Some("changelog.md"));
        // returning to connect switches screen
        app.back_to_connect();
        assert_eq!(app.screen, Screen::Connect);
    }

    #[test]
    fn engine_backed_open_and_select_loads_content() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("README.md"), b"# Hi\n").unwrap();
        std::fs::create_dir(dir.path().join("notes")).unwrap();
        std::fs::write(dir.path().join("notes").join("a.md"), b"alpha\n").unwrap();

        let eng = Engine::with_identity(Identity::from_seed(&[7u8; 32])).unwrap();
        let info = eng.add_local_folder(dir.path()).unwrap();
        let id = info.id.clone();

        let mut app = AspApp { engine: Some(Rc::new(eng)), ..AspApp::fixture_base(Theme::light()) };
        app.refresh_vaults();
        assert!(app.connect_rows.iter().any(|r| r.id == id));

        app.open_vault(&id);
        assert_eq!(app.screen, Screen::Editor);
        assert!(!app.files.is_empty());
        // README is the default selection; its content loads.
        assert_eq!(app.active.as_deref(), Some("README.md"));
        assert_eq!(app.content, "# Hi\n");

        // selecting another file loads its content + opens a tab.
        app.select_file("notes/a.md");
        assert_eq!(app.content, "alpha\n");
        assert!(app.tabs.iter().any(|t| t == "notes/a.md"));
    }

    #[test]
    fn engine_backed_file_operations() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("README.md"), b"# Hi\n").unwrap();
        let eng = Engine::with_identity(Identity::from_seed(&[11u8; 32])).unwrap();
        let info = eng.add_local_folder(dir.path()).unwrap();
        let id = info.id.clone();
        let mut app = AspApp { engine: Some(Rc::new(eng)), ..AspApp::fixture_base(Theme::light()) };
        app.refresh_vaults();
        app.open_vault(&id);

        // new_file creates untitled.md, selects it, opens a tab.
        app.new_file();
        assert_eq!(app.active.as_deref(), Some("untitled.md"));
        assert!(app.files.iter().any(|(p, _)| p == "untitled.md"));
        assert!(app.tabs.iter().any(|t| t == "untitled.md"));

        // a second new_file gets a unique name.
        app.new_file();
        assert!(app.files.iter().any(|(p, _)| p == "untitled-1.md"));

        // rename remaps the active file + tab.
        app.rename_file("untitled-1.md", "renamed.md");
        assert!(app.files.iter().any(|(p, _)| p == "renamed.md"));
        assert!(!app.files.iter().any(|(p, _)| p == "untitled-1.md"));

        // delete drops the file + its tab.
        app.delete_file("untitled.md");
        assert!(!app.files.iter().any(|(p, _)| p == "untitled.md"));
        assert!(!app.tabs.iter().any(|t| t == "untitled.md"));
    }

    #[test]
    fn engine_backed_time_travel_and_restore() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("README.md"), b"v1\n").unwrap();
        let eng = Engine::with_identity(Identity::from_seed(&[13u8; 32])).unwrap();
        let info = eng.add_local_folder(dir.path()).unwrap();
        let id = info.id.clone();
        let mut app = AspApp { engine: Some(Rc::new(eng)), ..AspApp::fixture_base(Theme::light()) };
        app.refresh_vaults();
        app.open_vault(&id);
        assert_eq!(app.content, "v1\n");
        assert!(!app.history_events.is_empty());

        let pre = app.history_events.iter().map(|e| (e.ts / 1000.0) as i64).max().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        app.engine.clone().unwrap().write_file(&id, "README.md", "v2\n").unwrap();
        app.load_history(&id);
        app.reload_active();
        assert_eq!(app.content, "v2\n");

        // travel to the pre-edit second → old content, read-only.
        app.time_travel_to(pre);
        assert!(app.is_time_travel());
        assert_eq!(app.content, "v1\n");

        // back to now → latest content.
        app.return_to_now();
        assert!(!app.is_time_travel());
        assert_eq!(app.content, "v2\n");

        // restore the old version → it becomes the live content.
        app.time_travel_to(pre);
        app.restore_version();
        assert!(!app.is_time_travel());
        assert_eq!(app.content, "v1\n");
    }

    #[test]
    fn engine_backed_share_opens_ticket_modal() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("README.md"), b"hi\n").unwrap();
        let eng = Engine::with_identity(Identity::from_seed(&[41u8; 32])).unwrap();
        let info = eng.add_local_folder(dir.path()).unwrap();
        let id = info.id.clone();
        let mut app = AspApp { engine: Some(Rc::new(eng)), ..AspApp::fixture_base(Theme::light()) };
        app.open_share(&id, "Research");
        match &app.modal {
            Modal::ShareVault { ticket, .. } => assert!(ticket.is_some(), "expected a ticket"),
            other => panic!("expected share modal, got {other:?}"),
        }
    }

    #[test]
    fn customize_modal_editing() {
        let mut a = AspApp::fixture_connect(Theme::light());
        a.open_customize("v1");
        match &a.modal {
            Modal::Customize { vault_id, buf, hue, .. } => {
                assert_eq!(vault_id, "v1");
                assert_eq!(buf.text, "Research Notes");
                assert_eq!(*hue, theme::VAULT_HUES[0]);
            }
            other => panic!("expected customize modal, got {other:?}"),
        }
        a.customize_backspace(); // delete trailing 's'
        a.customize_type("X");
        a.customize_set_hue(158.0);
        if let Modal::Customize { buf, hue, .. } = &a.modal {
            assert_eq!(buf.text, "Research NoteX");
            assert_eq!(*hue, 158.0);
        } else {
            panic!("modal changed unexpectedly");
        }
    }

    #[test]
    fn connect_input_editing() {
        let mut a = AspApp::fixture_connect(Theme::light());
        a.open_connect();
        assert!(matches!(a.modal, Modal::ConnectVault { .. }));
        a.connect_type("asp1");
        a.connect_type("abc");
        assert_eq!(a.connect_ticket().as_deref(), Some("asp1abc"));
        a.connect_backspace();
        assert_eq!(a.connect_ticket().as_deref(), Some("asp1ab"));
        a.close_modal();
        assert_eq!(a.connect_ticket(), None);
    }

    #[test]
    fn engine_backed_remove_vault_via_modal() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("README.md"), b"hi\n").unwrap();
        let eng = Engine::with_identity(Identity::from_seed(&[15u8; 32])).unwrap();
        let info = eng.add_local_folder(dir.path()).unwrap();
        let id = info.id.clone();
        let mut app = AspApp { engine: Some(Rc::new(eng)), ..AspApp::fixture_base(Theme::light()) };
        app.refresh_vaults();
        assert_eq!(app.connect_rows.len(), 1);

        app.open_remove(&id, "README");
        assert!(matches!(app.modal, Modal::RemoveVault { .. }));
        app.toggle_remove_trash();
        app.confirm_remove();
        assert_eq!(app.modal, Modal::None);
        assert!(app.connect_rows.is_empty());
        assert!(app.engine.clone().unwrap().list_vaults().is_empty());
    }

    #[test]
    fn engine_backed_editing_types_and_saves() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.md"), b"hello").unwrap();
        let eng = Engine::with_identity(Identity::from_seed(&[21u8; 32])).unwrap();
        let info = eng.add_local_folder(dir.path()).unwrap();
        let id = info.id.clone();
        let mut app = AspApp { engine: Some(Rc::new(eng)), ..AspApp::fixture_base(Theme::light()) };
        app.refresh_vaults();
        app.open_vault(&id);
        assert_eq!(app.content, "hello");

        app.begin_edit();
        assert!(app.editing);
        app.type_text(" world");
        assert_eq!(app.content, "hello world");
        // edits persist to the engine immediately.
        assert_eq!(app.engine.clone().unwrap().read_file(&id, "a.md").unwrap(), "hello world");

        app.editor_backspace();
        assert_eq!(app.content, "hello worl");
        app.editor_newline();
        assert_eq!(app.content, "hello worl\n");

        // navigating away exits edit mode.
        app.select_file("a.md");
        assert!(!app.editing);

        // can't edit while time-travelling.
        app.time_travel_to(0);
        app.begin_edit();
        assert!(!app.editing);
    }

    #[test]
    fn add_local_folder_path_opens_vault() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("README.md"), b"# Hi\n").unwrap();
        let eng = Engine::with_identity(Identity::from_seed(&[31u8; 32])).unwrap();
        let mut app = AspApp { engine: Some(Rc::new(eng)), ..AspApp::fixture_base(Theme::light()) };
        app.add_local_folder_path(dir.path());
        assert_eq!(app.screen, Screen::Editor);
        assert!(app.files.iter().any(|(p, _)| p == "README.md"));
        assert_eq!(app.content, "# Hi\n");
    }

    #[test]
    fn poll_refresh_picks_up_external_edits() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("README.md"), b"v1\n").unwrap();
        let eng = Engine::with_identity(Identity::from_seed(&[51u8; 32])).unwrap();
        let info = eng.add_local_folder(dir.path()).unwrap();
        let id = info.id.clone();
        let mut app = AspApp { engine: Some(Rc::new(eng)), ..AspApp::fixture_base(Theme::light()) };
        app.refresh_vaults();
        app.open_vault(&id);
        assert_eq!(app.content, "v1\n");

        // a "peer" edit lands via the engine; poll picks it up.
        app.engine.clone().unwrap().write_file(&id, "README.md", "v2-from-peer\n").unwrap();
        app.poll_refresh();
        assert_eq!(app.content, "v2-from-peer\n");

        // while editing, poll does not clobber the buffer.
        app.begin_edit();
        app.engine.clone().unwrap().write_file(&id, "README.md", "v3\n").unwrap();
        app.poll_refresh();
        assert_eq!(app.content, "v2-from-peer\n");
    }

    #[test]
    fn sidebar_drag_clamps() {
        let mut a = AspApp::fixture_editor(Theme::light());
        a.start_sidebar_drag();
        a.drag_sidebar(100.0);
        assert_eq!(a.sidebar_w, 200.0); // min
        a.drag_sidebar(900.0);
        assert_eq!(a.sidebar_w, 460.0); // max
        a.drag_sidebar(320.0);
        assert_eq!(a.sidebar_w, 320.0);
        a.end_drag();
        a.drag_sidebar(100.0); // not dragging → unchanged
        assert_eq!(a.sidebar_w, 320.0);
    }

    #[test]
    fn hist_drag_clamps() {
        let mut a = AspApp::fixture_editor(Theme::light());
        a.start_hist_drag();
        a.drag_hist(10.0);
        assert_eq!(a.hist_bar_h, 96.0); // min
        a.drag_hist(9999.0);
        assert_eq!(a.hist_bar_h, 640.0); // max
        a.drag_hist(300.0);
        assert_eq!(a.hist_bar_h, 300.0);
        a.end_drag();
        a.drag_hist(10.0);
        assert_eq!(a.hist_bar_h, 300.0); // not dragging → unchanged
    }

    #[test]
    fn tab_close_variants() {
        let base = AspApp::fixture_editor(Theme::light());
        // close others → only the kept tab + it's active
        let mut a = AspApp { ..AspApp::fixture_editor(Theme::light()) };
        a.close_others("notes/ideas.md");
        assert_eq!(a.tabs, vec!["notes/ideas.md".to_string()]);
        assert_eq!(a.active.as_deref(), Some("notes/ideas.md"));
        // close to right of README → only README
        let mut a = AspApp { ..AspApp::fixture_editor(Theme::light()) };
        a.close_to_right("README.md");
        assert_eq!(a.tabs, vec!["README.md".to_string()]);
        // close to left of last → only last; active (was README) reconciles to it
        let mut a = AspApp { ..AspApp::fixture_editor(Theme::light()) };
        a.close_to_left("notes/todo.md");
        assert_eq!(a.tabs, vec!["notes/todo.md".to_string()]);
        assert_eq!(a.active.as_deref(), Some("notes/todo.md"));
        // close all → empty, no active
        let mut a = AspApp { ..AspApp::fixture_editor(Theme::light()) };
        a.close_all_tabs();
        assert!(a.tabs.is_empty());
        assert_eq!(a.active, None);
        let _ = base;
    }

    #[test]
    fn toggle_theme_flips_appearance() {
        let mut app = AspApp::fixture_connect(Theme::light());
        assert_eq!(app.theme.appearance, crate::theme::Appearance::Light);
        app.toggle_theme();
        assert_eq!(app.theme.appearance, crate::theme::Appearance::Dark);
        app.toggle_theme();
        assert_eq!(app.theme.appearance, crate::theme::Appearance::Light);
    }
}
