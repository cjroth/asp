//! `AspApp` — the stateful root entity. Owns the engine handle and all UI state
//! (current screen, open vault, file tree, tabs, active file + content), and
//! dispatches rendering to the data-driven `screens::{connect,editor}` modules.
//! Interactivity (clicks) mutates this state and calls `cx.notify()`.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{prelude::*, AnyElement};

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
    pub id: String, // engine session id (for opening)
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
            vault_id: None,
            vault_name: String::new(),
            vault_hue: theme::VAULT_HUES[0],
            peers: 0,
            files: Vec::new(),
            expanded: HashMap::new(),
            tabs: Vec::new(),
            active: None,
            content: String::new(),
        };
        app.refresh_vaults();
        app
    }

    /// Fixture Connect screen (no engine) — for screenshots / visual checks.
    pub fn fixture_connect(theme: Theme) -> Self {
        let rows = vec![
            ConnectRow {
                id: "v1".into(),
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
            vault_id: None,
            vault_name: String::new(),
            vault_hue: theme::VAULT_HUES[0],
            peers: 0,
            files: Vec::new(),
            expanded: HashMap::new(),
            tabs: Vec::new(),
            active: None,
            content: String::new(),
        }
    }

    /// Rebuild the Connect rows from live engine data.
    pub fn refresh_vaults(&mut self) {
        let Some(eng) = self.engine.clone() else { return };
        let meta: VaultMetaMap = HashMap::new(); // persisted meta wired later
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
            let m = vault_meta::resolve_meta(&meta, &v.vault_id, &base);
            rows.push(ConnectRow {
                id: v.id.clone(),
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
        self.screen = Screen::Editor;
    }

    /// Select a file in the tree → load its content + open a tab.
    pub fn select_file(&mut self, path: &str) {
        self.active = Some(path.to_string());
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

    /// Return to the Connect screen.
    pub fn back_to_connect(&mut self) {
        self.refresh_vaults();
        self.screen = Screen::Connect;
    }

    pub fn active_path(&self) -> Option<&str> {
        self.active.as_deref()
    }
}

impl Render for AspApp {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let el: AnyElement = match self.screen {
            Screen::Connect => connect::render(self, cx).into_any_element(),
            Screen::Editor => editor::render(self, cx).into_any_element(),
        };
        el
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
}
