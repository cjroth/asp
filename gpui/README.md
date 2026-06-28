# aspgui — a native Rust + GPUI clone of `asp/desktop`

A from-scratch reimplementation of the ASP **Context Desktop** vault editor
(`/home/chris/asp/desktop`, normally Tauri + React) as a **native GPUI app**.

It keeps the original's **HARD INVARIANT**: no protocol / merge / history /
identity logic lives in the UI. Every behavior is a call into
`asp-desktop-engine` → `asp-core` (linked directly, the same engine the Tauri
shell uses). aspgui is just a new native *shell* over that engine.

## What's implemented

- **Connect screen** — logo + wordmark, "Your vaults", New/Connect buttons,
  recent-vaults list (real vaults from the engine, persisted across runs via
  `reopen_saved`), device fingerprint, theme toggle.
- **Editor screen** — sidebar (vault switcher → back to connect, file tree),
  tab bar (active-tab accent indicator + close), header (save status, live word
  count, Share, theme toggle).
- **WYSIWYG Markdown rendering** (`markdown.rs`) — headings, bullet/task/ordered
  lists, blockquotes, fenced code, rules, and inline **bold**/*italic*/`code`/
  links via `StyledText` highlight runs. Prose in **Newsreader** serif, code in
  **JetBrains Mono** (matches the design).
- **History time-travel bar** — path/fingerprint row, History/Log switcher,
  event count, a timeline track with per-kind colored event dots and a playhead.
  Click a dot → view the vault as of that moment (read-only), with a banner to
  **Restore this version** or **Return to now**. Backed by the engine's
  `history` / `read_file_at` / `restore_file_at`.
- **Share modal** — generates a real iroh share ticket via
  `set_allow_connections`.
- **Interactive editing** — click the content pane to enter source-edit mode:
  a caret + keyboard input (`on_key_down` / `key_char`, arrows/home/end/enter/
  backspace), rendered one row per source line (the original's 1:1 line↔div
  invariant). Edits write through `write_file`, so the history timeline grows
  live; Esc / clicking away returns to the rendered view.
- **Light + dark themes** — full token set ported from the design's CSS vars.

Possible future polish: WYSIWYG-while-typing (the original's contentEditable
rich editor), context menus, customize/remove modals, and the native folder
dialog for New/Connect.

## Layout

```
src/
  theme.rs       color tokens (light/dark) from the design's styles.css
  backend.rs     thin wrapper over asp-desktop-engine (the only path to behavior)
  markdown.rs    read-only "live" Markdown → GPUI elements
  app.rs         AspApp root view: screen/selection/history state + all handlers
  bin/
    aspgui.rs    production entrypoint (real window)
    shoot.rs     headless scripted screenshot driver (the feedback loop)
    smoke.rs     minimal render smoke test
```

## The feedback loop (the "webkit cli" equivalent)

GPUI renders via wgpu; under Xvfb + a software Vulkan ICD (lavapipe) the
swapchain *present* never reaches a readable framebuffer, so X11 screen-grabs
come back black. Instead, the local GPUI checkout at `/home/chris/zed-src` is
patched to render a frame to an **offscreen texture and read the pixels back**
(`WgpuRenderer::render_to_image` → `X11Window::render_to_image` →
`Window::capture_image()`), bypassing present entirely.

`bin/shoot.rs` drives the real app through a scripted scenario (open vault →
select files → time-travel → share → toggle theme), forcing a synchronous
`window.draw()` and capturing `capture_image()` to a PNG after each step. The
agent then *reads the PNGs* to verify the UI visually — real rendered output,
not a DOM/text assertion.

The harness lives in the `shoot`/`smoke` binaries, gated behind the `capture`
feature so a normal app build never touches the patch (see below).

## Run the app (macOS / Metal, or Linux with a display)

The app uses **stock GPUI** from Zed git — no patch needed to run it.

Prerequisites on macOS: Rust via rustup + Xcode Command Line Tools
(`xcode-select --install`).

```sh
git fetch && git checkout gpui      # this branch
cd gpui
cargo run --bin aspgui              # builds lib + app only; Metal on macOS
```

Use `--bin aspgui` (not bare `cargo build`/`run`): the `shoot`/`smoke` binaries
require the `capture` feature and the patched GPUI, so they're skipped by a
normal build.

## Headless screenshot harness (the feedback loop)

To run the PNG-capture harness you need GPUI with the offscreen
`render_to_image` patch:

```sh
git clone https://github.com/zed-industries/zed /path/to/zed
cd /path/to/zed && git checkout 5837e7e && git apply /path/to/asp/gpui/zed-gpui-headless.patch
```

Point the two GPUI deps in `Cargo.toml` at `/path/to/zed/crates/{gpui,gpui_platform}`
(add `features = ["x11","wayland"]` to `gpui_platform` on Linux), then:

```sh
cargo build --bin shoot --features capture
bash shot-driver.sh /tmp/aspshots   # Linux: headless under Xvfb + lavapipe
# macOS has a built-in Metal headless renderer, so capture works there too.
# → /tmp/aspshots/{01-connect,02-editor,03-welcome,04-timetravel,05-share,
#   06-editor-dark,07-connect-dark,08-editing,09-edited-rendered}.png
```
