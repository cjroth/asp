# gpui gotchas & idioms (Zed UI framework)

Hard-won notes building a real gpui app. Verified against zed rev `5837e7e`
(gpui 0.2.2, the `gpui_platform`/`gpui_wgpu`/`gpui_linux` split). Most are stable
idioms, but APIs move between revs — when something doesn't resolve, read the
crate source at `~/.cargo/git/checkouts/zed-*/<rev>/crates/gpui/src` (it's the
ground truth) rather than guessing from memory.

## Entry point & platform
- Run with `gpui_platform::application().run(|cx: &mut App| { ... })`.
  `Application::new()` does NOT exist in this rev — that split moved the platform
  entry into the `gpui_platform` crate (`application()` / `headless()`).
- Set an asset source for `svg()`/fonts: `application().with_assets(MyAssets).run(..)`.
- `examples/` in the gpui crate are the best API reference (`hello_world.rs`,
  `input.rs`, `popover.rs`, `text.rs`, `uniform_list.rs`).

## Styling (Tailwind-like, macro-generated)
- Generic value setters exist alongside the scale steps: `.w(px(266.))`,
  `.h(px(47.))`, `.p(px(20.))`, `.px(..)`, `.gap(px(11.))`, `.rounded(px(11.))`,
  `.mt(..)`, `.min_w(px(0.))`, etc. — pass `px(value)`.
- Borders are fixed-width helpers: `.border_1()`, `.border_t_2()`, `.border_l_2()`,
  `.border_color(impl Into<Hsla>)`.
- Colors: `rgb(0xRRGGBB)`, `rgba(0xRRGGBBAA)`, `hsla(h,s,l,a)` (h is 0..1, i.e.
  `hue/360.0`). `bg(..)`/`text_color(..)` take `impl Into<Hsla>`; `Rgba: Into<Hsla>`.
- `FontWeight(550.0)` or the consts `FontWeight::NORMAL/MEDIUM/SEMIBOLD/BOLD`.
- No letter-spacing/tracking helper — approximate or omit.
- `Pixels.0` is private → convert with `f32::from(pixels)`.

## The `Stateful<Div>` return-type trap
Adding `.id(...)`, `.on_click`, `.on_mouse_down`, `.track_focus`, `.overflow_*_scroll`,
or `.cursor_*_resize` turns `Div` into `Stateful<Div>`. A helper `fn foo() -> Div`
then fails to compile. Make such helpers return `impl IntoElement`. To unify
different branches/screens, erase to `AnyElement` via `.into_any_element()`.

## Interactivity
- `on_click` requires the element be stateful: call `.id(x)` BEFORE `.on_click(..)`.
- Handlers: `.on_click(cx.listener(|this, _ev, window, cx| { this.mutate(); cx.notify(); }))`.
  Keep state-mutating methods cx-free (testable) and call `cx.notify()` in the listener.
- Right-click: `.on_mouse_down(MouseButton::Right, cx.listener(|this, ev: &MouseDownEvent, _, cx| {...}))`;
  position via `f32::from(ev.position.x)`.
- Drag: `on_mouse_move`/`on_mouse_up` belong on a ROOT element so they keep firing
  while the pointer leaves the handle. `window.viewport_size()` gives window dims.
- Focus: a view holds `Option<FocusHandle>`; create it lazily in `render`
  (`self.focus.get_or_insert_with(|| cx.focus_handle())` — `cx.focus_handle()` needs
  the context, not available in cx-free constructors). Focus with `window.focus(&h, cx)`.
- Key input: focusable element + `.on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {...}))`.
  Use `ev.keystroke.key` ("backspace"/"enter"/"left"...) and `ev.keystroke.key_char`
  (printable). Clipboard paste: `cx.read_from_clipboard()?.text()`.

## Overlays (menus & modals)
- `deferred(anchored().position(point(px(x),px(y))).child(card.on_mouse_down_out(..)))`
  is the popover pattern (`examples/popover.rs`).
- **`deferred()` is NOT flushed in the single-frame headless `--shot` capture**, so
  anchored menus won't appear in screenshots (they work in the live app). For
  things you must screenshot (modals), skip `deferred`: render a full-screen
  `div().absolute().top_0().left_0().size_full().bg(overlay).flex().items_center()
  .justify_center().child(card)` as the LAST child of the root — and make the root
  `.relative()` (absolute positions against the nearest positioned ancestor, else
  it won't cover). `on_mouse_down_out` on the card dismisses.

## Text / rich content
- Multi-style inline text with wrapping: `StyledText::new(s).with_runs(vec![TextRun{
  len: bytes, font, color, background_color, underline, strikethrough }])`. `TextRun`
  derives `Default`. Run length is in UTF-8 BYTES and must cover the whole string.
- Per-run size is NOT in `TextRun`; it comes from the ambient `.text_size(..)` on an
  ancestor. Set font family/weight/style on the run's `Font` (`font(name)`, then set
  `.weight`/`.style`).
- `svg().path("icons/x.svg").size(px(16.)).text_color(c)` renders a bundled SVG as an
  alpha mask tinted by `text_color` (the SVG's own colors are ignored — fill/stroke
  with anything). Needs an `AssetSource` that returns the svg bytes by path.
- System fonts: `gpui_wgpu::CosmicTextSystem::new("fallback")` loads them (so
  installed families like "JetBrains Mono" resolve). Bundle exact fonts via
  `cx.text_system().add_fonts(vec![Cow<[u8]>])` for determinism.

## Async / timers
- `cx.spawn(async move |this /* WeakEntity */, cx| { ...; this.update(cx, |this, cx| {..}).ok(); })`.
- Periodic work: capture `cx.background_executor().clone()` and
  `bg.timer(Duration::from_secs(2)).await` in a `loop`. (Test dispatcher won't fire
  timers without `advance_clock`, so this is inert in `--shot` — fine.)
- Native file picker: `cx.prompt_for_paths(PathPromptOptions{files,directories,multiple,prompt})`
  returns a `oneshot::Receiver`; await it in `cx.spawn`.

## Testing on Linux (works — not macOS-only)
- `HeadlessAppContext` (real text + your offscreen renderer) runs on Linux for
  render/screenshot.
- `#[gpui::test] fn t(cx: &mut TestAppContext)` + `cx.open_window(size, |_,_| View)` +
  `VisualTestContext::from_window(window.into(), cx).simulate_click(point, Modifiers::default())`
  dispatches a REAL click through the event system and asserts resulting state —
  runs on Linux worker threads in practice (despite older "macOS main thread" notes).

## Build/runtime hygiene
- `cmake` required. `cargo build -j3`/`-j4` (avoid OOM at link).
- Run headless with `VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json` (lavapipe).
- `pkill -x <app>`, never `pkill -f target/debug/<app>` (the latter matches your own
  shell's command line and kills it → exit 143/144).
