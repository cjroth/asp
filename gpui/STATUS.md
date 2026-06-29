# ASP gpui port — status

Native Rust/gpui port of the Context Desktop vault editor (`../desktop`). Goal:
1:1 design + feature parity, performance parity, automated verification.

## Run / test / screenshot
```bash
cd gpui
cargo build -j4                              # build (needs cmake; vendored zed at /home/chris/asp/.gpui-vendor/zed)
cargo test -j4                               # 74 tests (pure-logic parity + engine + app behavior)
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
- Clean build: **0 warnings, 92 tests**. Screenshots for every screen/modal in `tools/shots/`.

## Remaining toward 100%
- **Live WYSIWYG-while-typing** — editing currently shows the raw source (with a caret) and saves;
  the desktop renders markdown *in place while typing* (contentEditable). This is the single hardest
  remaining piece (≈ Zed-editor-scale) and is the main gap.
- **UI prefs persistence** (theme / sidebar width / history height) — vault meta persists; prefs don't yet.
- **Desktop reference pixel-diff** — capture desktop screens (needs a browser/macOS) and diff vs `--shot`.
- Broader **e2e** via gpui `VisualTestContext` (simulate real click/key dispatch, assert).
- Deferred: per-language code syntax highlighting, mermaid/diagrams, frontmatter styles, drag-reorder tabs.
