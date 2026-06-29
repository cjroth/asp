# ASP gpui port — status

Native Rust/gpui port of the Context Desktop vault editor (`../desktop`). Goal:
1:1 design + feature parity, performance parity, automated verification.

## Run / test / screenshot
```bash
cd gpui
cargo build -j4                              # build (needs cmake; vendored zed at /home/chris/asp/.gpui-vendor/zed)
cargo test -j4                               # 97 tests (pure-logic parity + engine + app behavior)
# headless pixel screenshot of any screen (no display needed):
VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json \
  ./target/debug/asp-gpui --shot out.png <screen>   # connect | connect-dark | editor | editor-dark
./target/debug/asp-gpui                      # live app (needs a display)
```

## Architecture
- `src/main.rs` — entry (`gpui_platform::application()`); `--shot` headless capture via `HeadlessAppContext`.
- `src/app.rs` — **`AspApp`**: the stateful root entity. Owns the engine + all UI state
  (screen, open vault, file tree, tabs, active file+content, theme). State methods are
  cx-free (testable); listeners call them + `cx.notify()`.
- `src/engine.rs` — wrapper over `asp_desktop_engine::DesktopEngine` (shared device identity
  at `~/.asp/id_ed25519`). Reuses the native backend; no protocol logic here.
- `src/theme.rs` — design tokens (light/dark palettes, fonts, accent hues).
- `src/assets.rs` + `assets/` — `AssetSource` for bundled SVG icons + fonts (JetBrains Mono, Newsreader).
- `src/icons.rs` — `icon(name, size, color)` → tinted `svg()`.
- `src/screens/{connect,editor}.rs` — data-driven, interactive render fns over `AspApp`.
- `src/vault/*.rs` — pure-logic modules ported 1:1 from desktop TS with parity tests:
  `format, tree, tabs, history, pretty_names, vault_meta, markdown, prefs, log`.
- `docs/` — `DESIGN_SPEC.md`, `FEATURE_SPEC.md`, `PLAN.md`.

## Done (verified — 84 tests pass)
- Headless **offscreen pixel capture** on Linux (wgpu render-to-texture; vendored+patched zed).
- **Connect** + **Editor** screens, light + dark, close visual match (screenshots in tools/shots).
- **Stateful interactive app**: open vault → editor, select file (loads content), tabs
  (open/close/neighbor-select), expand/collapse folders, back to connect, theme toggle.
- **File ops** (new/rename/delete) — engine-backed, tab/selection remap.
- **Live markdown render**: headings, bold/italic, inline code, links, bullet/ordered lists,
  task checkboxes, blockquotes, code fences (via `StyledText`+`TextRun`).
- **Text editing**: click prose → edit raw source with caret; type/backspace/delete/enter/arrows;
  saves to engine immediately (pure `TextBuffer` core, tested).
- **History time-travel**: clickable event ticks, read-only banner, "Restore"/"Return to now".
- **Overlays**: vault context menu (right-click) + Remove-vault modal; **New Vault** native folder picker.
- 10 pure-logic modules ported 1:1 with parity tests; engine round-trip + app-behavior tests.

### Also done since
- **History time-travel** (clickable ticks, read-only banner, restore).
- **All modals**: Remove, Share (real ticket), **Connect** (ticket input → clone_remote), and
  **Customize** (name + hue swatches, persisted to `~/.asp/desktop_vaultmeta.json`).
- **Context menus**: vault (right-click), tab (close others/left/right/all), file (new/delete).
- **New Vault** + **Connect Vault** native folder pickers; **live-sync poll** (2s);
  **sidebar + history-bar resize**; **perf harness** (`--perf`).
- **Code syntax highlighting** in fenced blocks; **drag-to-reorder tabs**; **live-preview editing**
  (styled markdown except the caret line); **UI prefs persistence**; **pixel-diff harness** (`tools/diff.sh`).
- Clean build: **0 warnings, 97 tests**. Screenshots for every screen/modal in `tools/shots/`.

## Remaining toward 100%
- **Inline per-token syntax reveal on the caret line** — editing is Obsidian-style live preview
  (styled markdown on every line except the caret line, which shows raw source + caret). The desktop
  reveals syntax inline within the rendered flow (contentEditable); matching that exactly is the last
  editor refinement.
- **UI prefs persistence** (theme / sidebar width / history height) — vault meta persists; prefs don't yet.
- **Desktop reference pixel-diff** — harness DONE (`tools/diff.sh A.png B.png`); capturing the desktop reference shots needs a browser/native app (run on macOS), as no browser is installable in this Linux sandbox.
- Broader **e2e** via gpui `VisualTestContext` (simulate real click/key dispatch, assert).
- Deferred (niche): mermaid/diagrams, YAML frontmatter property styles. (Code syntax highlighting,
  drag-reorder tabs, live-preview editing, prefs persistence are now DONE.)
