# ASP gpui port — build & verification plan

Native Rust/gpui port of the Context Desktop vault editor (`/home/chris/asp/desktop`),
targeting 1:1 design + feature parity, performance parity, and automated guarantees.

## Foundation (PROVEN)
- `gpui` rev `5837e7ef…` (v0.2.2) builds on this box. Entry: `gpui_platform::application()`.
  Renderer is **wgpu** (not blade); on Linux it selects llvmpipe/lavapipe Vulkan.
- App links `asp-desktop-engine` (path) → `asp-core` natively. **No backend reimplementation** —
  reuse `list_files/read_file/write_file/rename/delete/history/read_file_at/restore_file_at/
  clone_remote/set_allow_connections/…` (see FEATURE_SPEC.md §1).
- Headless run works under Xvfb + lavapipe: window maps at correct geometry, no panics.
  Harness: `tools/harness.sh` (asp_env/asp_launch/asp_shot/asp_kill), display `:77`.
- Capture pipeline verified (xeyes renders & screenshots fine).

## Known constraint: pixel capture on Linux
- wgpu/Vulkan present to **Xvfb** does not reach a readable framebuffer (lavapipe wants DRI3;
  Xvfb lacks it; `MESA_VK_WSI_DEBUG=sw` did not fix it). So `import -window root` is black
  even though the app renders.
- gpui's offscreen `render_to_image` / `PlatformHeadlessRenderer` is implemented **only for
  macOS Metal** in this rev (`gpui_platform::current_headless_renderer()` returns `None` on Linux).
- Options for real pixels:
  - **macOS:** free — Metal headless renderer → `capture_screenshot()` works; real windows work.
  - **Linux:** patch a wgpu offscreen renderer into a local zed clone (sizable; full recompile).

## Verification strategy (dual track)
1. **Behavior/correctness (Linux-OK, mathematical guarantee):**
   - Engine integration tests (reuse/extend `desktop/engine/tests`) for file/history/sync logic.
   - Port pure-logic modules (markdown, tree, tabs, history geometry, prefs, format) to Rust with
     unit tests mirroring the desktop vitest suites (1:1 fixtures) → proves logic parity.
   - gpui `HeadlessAppContext`/`VisualTestContext` (TestPlatform + cosmic text) layout/structure
     tests: assert element tree, text, and bounds — works on Linux now.
2. **Pixel-perfect visual parity:**
   - gpui snapshot tests via `capture_screenshot()` → PNG, compared to desktop reference shots.
     Run on macOS (or Linux once the offscreen renderer is patched in).
   - Reference shots: drive the desktop app (`desktop/e2e`) to capture the same screens/states.
3. **Performance:** mirror `desktop/perf-harness` methodology (frame timing, large-vault scroll,
   prose typing latency) against the gpui app.

## App architecture (planned)
- `src/app.rs` — root view, screen/selection state, engine handle, polling tasks.
- `src/theme.rs` — design tokens from DESIGN_SPEC.md (colors, fonts, spacing, radii) as a `Theme`.
- `src/engine.rs` — thin async wrapper over `asp_desktop_engine::DesktopEngine`.
- `src/screens/{connect,editor}.rs` — the two screens.
- `src/components/{sidebar,file_tree,tab_bar,live_editor,history_bar,modals,context_menu,icons}.rs`.
- `src/vault/{markdown,tree,tabs,history,prefs,format,...}.rs` — ported pure logic + tests.
- Assets: self-host JetBrains Mono + Newsreader woff2/ttf (copy from desktop) for text parity.

## Status / next steps
- [x] Scaffold crate, prove gpui build+run+capture-pipeline, write specs, harness.
- [ ] Decide pixel-capture track (macOS vs Linux wgpu-offscreen patch).
- [ ] Theme module + Connect screen (first visual milestone).
- [ ] Editor screen skeleton (sidebar + tabs + editor pane).
- [ ] Port pure-logic modules with parity tests.
- [ ] Live editor (1 line ↔ 1 block invariant), history scrubber, modals, context menus.
- [ ] Engine wiring + polling + live sync.
- [ ] Snapshot + perf harness; iterate to parity.
