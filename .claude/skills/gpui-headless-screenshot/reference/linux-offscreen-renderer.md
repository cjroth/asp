# Linux offscreen wgpu renderer for gpui (one-time patch)

On macOS, `gpui_platform::current_headless_renderer()` returns a Metal headless
renderer and `capture_screenshot()` just works. On Linux it returns `None` (the
offscreen renderer is macOS-only upstream), so you add a wgpu equivalent. gpui's
Linux renderer is already `wgpu`, and wgpu does offscreen render-to-texture +
readback trivially — gpui just doesn't expose it. This patch wires it.

## 0. Prereqs
- `cmake` (zed build needs it): `apt-get install -y cmake`.
- A software Vulkan ICD so it runs with no GPU — **lavapipe**
  (`/usr/share/vulkan/icd.d/lvp_icd.json`, package `mesa-vulkan-drivers`), plus
  `libvulkan.so.1`. Select it at runtime with
  `VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json`.
- Build with `cargo build -j3` / `-j4`. Higher `-j` peaks memory during the final
  link and OOMs with the cryptic `failed to map object file: memory map must have a
  non-zero length`.

## 1. Vendor zed (so you can patch it)
Copy the cargo git checkout to a writable dir and point your deps at it:
```bash
cp -a ~/.cargo/git/checkouts/zed-*/<shortrev> /path/to/vendor/zed
```
In your app's `Cargo.toml`, switch from `git=...` to `path=...` for `gpui` +
`gpui_platform` (transitive zed crates resolve from the vendored workspace), and
add the `test-support` feature:
```toml
gpui          = { path = "/path/to/vendor/zed/crates/gpui", features = ["test-support"] }
gpui_platform = { path = "/path/to/vendor/zed/crates/gpui_platform", features = ["font-kit","wayland","x11","test-support"] }
gpui_wgpu     = { path = "/path/to/vendor/zed/crates/gpui_wgpu", features = ["font-kit"] }
image = "0.25"
```
Delete the stale `Cargo.lock` so it re-resolves. (Keep this Linux-only; gate it
`[target.'cfg(not(target_os="macos"))'.dependencies]` and use upstream `git=` on macOS.)

## 2. Patch `gpui_wgpu` (~150 lines, all in the vendor)
The trait you implement (gpui core, gated `any(test, feature="test-support")`):
```rust
pub trait PlatformHeadlessRenderer {
    fn render_scene_to_image(&mut self, scene: &Scene, size: Size<DevicePixels>) -> Result<RgbaImage>;
    fn render_scene(&mut self, scene: &Scene, size: Size<DevicePixels>) -> Result<()>;
    fn sprite_atlas(&self) -> Arc<dyn PlatformAtlas>;
}
```

`crates/gpui_wgpu/src/wgpu_context.rs`:
- add `new_headless(instance)` — request an adapter with `compatible_surface: None`
  (the existing `new()` already passes `None`), then reuse the device-creation path.

`crates/gpui_wgpu/src/wgpu_renderer.rs`:
- make `WgpuResources.surface` an `Option<wgpu::Surface<'static>>`; thread
  `if let Some(surface)` through `new_internal` (configure), `draw()` (early-return
  if `None`), resize/recover. The window path keeps `Some(..)` — behavior unchanged.
- factor the per-batch encode loop out of `draw()` into
  `fn encode_scene(&mut self, scene, target_view)` (no `present`), called by both.
- add (gated `test-support`): `new_headless(size)` (fixed format **`Rgba8Unorm`**,
  `Opaque`, builds all pipelines with `surface: None`, owns an offscreen target +
  intermediate textures), and `render_scene_to_image(scene, size)` — encode to an
  owned `RENDER_ATTACHMENT|COPY_SRC` `Rgba8Unorm` texture, then
  `copy_texture_to_buffer` (respect 256-byte `bytes_per_row` alignment — pad/unpad),
  `device.poll(Wait)`, `map_async`, copy out tight RGBA into `image::RgbaImage`.
- add `pub struct WgpuHeadlessRenderer { renderer, atlas }` impl
  `PlatformHeadlessRenderer`; `sprite_atlas()` returns the renderer's own atlas
  (so glyphs the window rasterizes land in the atlas the capture reads).
- re-export `WgpuHeadlessRenderer` from `src/gpui_wgpu.rs` (gated).

`crates/gpui_linux/src/gpui_linux.rs`: add (gated `test-support`)
`pub fn headless_renderer(size) -> Option<Box<dyn PlatformHeadlessRenderer>>`
building a `WgpuHeadlessRenderer`. Add `test-support` to its Cargo features so it
enables `gpui_wgpu?/test-support`.

`crates/gpui_platform/src/gpui_platform.rs`: make `current_headless_renderer()`
return `gpui_linux::headless_renderer(<seed size, e.g. 2048x1536>)` on
linux/freebsd (it resizes per-scene). macOS branch unchanged. Add `test-support`
to its features so it enables `gpui_linux/test-support`.

## 3. Verify
`VK_ICD_FILENAMES=.../lvp_icd.json ./target/debug/<app> --shot out.png` →
`convert out.png -format 'colors=%k mean=%[fx:mean]\n' info:` should show many
colors and a nonzero mean (not a solid black/transparent frame).

## Pitfalls (learned the hard way)
- **`Rgba8UnormSrgb` target double-gamma-brightens** colors (`0x0d`→`0x40`). Use
  non-sRGB `Rgba8Unorm`; it matches the windowed `Bgra8Unorm` path and needs no swizzle.
- The headless main pass clears to **transparent**; rely on the root view filling
  the window with an opaque background quad (full-bleed UIs are fine).
- A harmless `EGL ... DRI2: failed to load driver` line prints at startup — that's
  the GL backend probe failing before the Vulkan (llvmpipe) adapter is selected.
