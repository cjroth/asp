---
name: gpui-headless-screenshot
description: Render any gpui (Zed's GUI framework) view/window to a PNG headlessly — no display or GPU required — so you can visually verify the UI, eval pixel output like a human, build snapshot tests, and iterate write→build→screenshot→look. Use whenever working on a gpui/Zed-style Rust app and you need to SEE what it renders (especially on a headless Linux box, CI, or a remote sandbox). Covers macOS (built-in Metal headless) and headless Linux (offscreen wgpu render-to-texture, including the renderer patch). Pairs with gpui-visual-diff for pixel-perfect comparison.
---

# Headless gpui screenshots

The goal: turn a gpui app into something that renders a chosen view to `out.png`
**without opening a real window**, so you can look at the actual pixels while
developing — even on a headless Linux box with no display and no GPU.

gpui can render a window's scene to an `RgbaImage` offscreen via
`Window::render_to_image()` / `HeadlessAppContext::capture_screenshot()`. The only
catch is that the offscreen renderer is **only wired up for macOS Metal** in the
zed source; on Linux you add a small wgpu offscreen renderer (one-time patch).

## Decision: which path?

- **macOS** — works out of the box. `gpui_platform::current_headless_renderer()`
  returns a Metal headless renderer. Just add the `--shot` mode below; no patch.
- **Headless Linux / CI / sandbox** — `current_headless_renderer()` returns `None`
  upstream, so you patch `gpui_wgpu` to add an offscreen renderer. See
  `reference/linux-offscreen-renderer.md`. After that, the same `--shot` code works.
  (This needs `cmake` and a software Vulkan ICD — lavapipe — both common/installable.)

Do NOT waste time trying to screenshot a *real* gpui window on headless Linux via
Xvfb: gpui presents through Vulkan WSI and lavapipe→Xvfb does not blit to a
readable framebuffer (it wants DRI3, which Xvfb lacks). Offscreen render-to-texture
is the reliable path and is what this skill sets up.

## Steps

1. **Add a `--shot <out.png> [view]` mode** to the app's `main.rs`. Copy
   `assets/shot_mode.rs` and adapt the view dispatch to your app's root view(s).
   It builds a `HeadlessAppContext`, opens an offscreen window, drives a frame, and
   saves `capture_screenshot()` to a PNG. Enable `test-support` on the `gpui` /
   `gpui_platform` deps (that feature gates `PlatformHeadlessRenderer`).

2. **Linux only:** apply the offscreen-renderer patch — see
   `reference/linux-offscreen-renderer.md` (vendor zed, ~150 lines in `gpui_wgpu`,
   wire `current_headless_renderer()`).

3. **Build & capture** (source the env helper for Linux):
   ```bash
   source <skill>/scripts/gpui-env.sh      # sets VK_ICD to lavapipe, etc. (Linux)
   cargo build -j4                          # use -j3/-j4: high -j OOMs (see env helper)
   ./target/debug/<app> --shot out.png editor
   ```
   Then `Read out.png` to look at it. Check it's non-blank:
   `convert out.png -format 'colors=%k mean=%[fx:mean]\n' info:` (colors>1, mean>0).

4. **Iterate:** edit UI → `cargo build` → `--shot` → Read the PNG → adjust.

## Gotchas worth knowing up front

See `reference/gpui-gotchas.md` for the full list. The ones that bite immediately:
- App entry is `gpui_platform::application().run(...)`, not `Application::new()`.
- Any element with `.id(...)`/`.on_click`/`.track_focus`/scroll becomes
  `Stateful<Div>` — a fn returning `Div` then fails; return `impl IntoElement`.
- Use texture format `Rgba8Unorm` (NOT `Rgba8UnormSrgb`) for readback or colors
  double-gamma and look washed out.
- `deferred()`/`anchored()` overlays are NOT flushed in the single-frame headless
  capture. For modals, render a full-screen `.absolute()` overlay as the **last
  child of a `.relative()` root** instead of `deferred()` (anchored context menus
  only show in the live app, which is fine — test their logic directly).
- Never `pkill -f target/debug/<app>` from a shell whose own command line contains
  that string — it kills the shell (exit 143/144). Use `pkill -x <app>`.

## Verifying behavior without pixels
For correctness (not looks), `HeadlessAppContext`/`TestAppContext` work on Linux:
`#[gpui::test]` + `cx.open_window(size, build)` + `VisualTestContext::from_window(...)`
+ `simulate_click(point, modifiers)` dispatches REAL clicks and runs on Linux — good
for end-to-end "click X → assert state" tests.
