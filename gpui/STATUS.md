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

## Done (verified)
- Headless **offscreen pixel capture** on Linux (wgpu render-to-texture; vendored+patched zed).
- **Connect** + **Editor** screens, light + dark, close visual match (screenshots in tools/shots).
- **Stateful interactive app**: open vault → editor, select file (loads content), tabs
  (open/close/neighbor-select), expand/collapse folders, back to connect, theme toggle,
  **file ops** (new/rename/delete) — all engine-backed.
- **Live markdown render**: headings, bold/italic, inline code, links, bullet/ordered lists,
  task checkboxes, blockquotes, code fences (via `StyledText`+`TextRun`).
- 9 pure-logic modules ported; **74 tests pass** (parity + engine round-trip + app behavior).

## Remaining toward 100%
- **Text editing surface** (type + save) — adapt gpui input example to multi-line; the core gap.
  Live WYSIWYG-while-typing (desktop uses contentEditable) is the hardest piece.
- **Modals + context menus** — Share/Remove/Customize (overlay) + file/tab/vault menus
  (`anchored()`/`deferred()`), native folder dialog for New/Connect vault.
- **History scrubber interactivity** — click/drag playhead, zoom, time-travel read
  (`read_file_at`) + restore (`restore_file_at`); geometry already ported in `vault/history.rs`.
- **Live sync → UI** — drain the engine change-receiver, repaint on peer edits.
- **Perf harness** (mirror `../desktop/perf-harness`) + **e2e/behavior tests** (gpui `VisualTestContext`).
- **Desktop reference pixel-diff** — capture desktop screens (browser/macOS) and diff vs `--shot`.
- Deferred: per-language code syntax highlighting, mermaid/diagrams, frontmatter styles.
