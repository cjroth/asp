// Drop-in `--shot <out.png> [view]` headless screenshot mode for a gpui app.
// Adapt `open_root` to your app's root view(s). Requires the `test-support`
// feature on the `gpui`/`gpui_platform` deps (gates `PlatformHeadlessRenderer`).
//
// Wire into main():
//     let args: Vec<String> = std::env::args().collect();
//     if let Some(i) = args.iter().position(|a| a == "--shot") {
//         run_shot(args.get(i+1).map(|s| s.as_str()).unwrap_or("shot.png"),
//                  args.get(i+2).map(|s| s.as_str()).unwrap_or("default"));
//         return;
//     }
//     gpui_platform::application().with_assets(Assets).run(|cx| { /* live app */ });

use std::sync::Arc;
use gpui::{px, size, AnyWindowHandle, HeadlessAppContext, Pixels, Size};

fn run_shot(out_path: &str, view: &str) {
    // A real text system so glyph shaping/measurement is accurate.
    // Linux: gpui_wgpu::CosmicTextSystem::new("sans-serif").
    // macOS: use the Metal text system instead (gpui_macos::MacTextSystem / the
    //        platform default) — gpui_wgpu may not be a dep there.
    let text_system = Arc::new(gpui_wgpu::CosmicTextSystem::new("sans-serif"));

    let mut cx = HeadlessAppContext::with_platform(
        text_system,
        Arc::new(Assets), // your gpui::AssetSource (or Arc::new(()) if none)
        gpui_platform::current_headless_renderer, // macOS: Metal; Linux: your wgpu patch
    );
    // If you bundle fonts, register them so the capture uses them:
    // cx.text_system().add_fonts(Assets::font_bytes()).ok();

    let win_size: Size<Pixels> = size(px(1100.0), px(740.0));
    let handle: AnyWindowHandle = open_root(&mut cx, win_size, view).into();

    // Drive layout + an initial draw, then refresh to guarantee a fresh frame.
    cx.run_until_parked();
    cx.update_window(handle, |_, window, _| window.refresh()).expect("refresh");
    cx.run_until_parked();

    let image = cx.capture_screenshot(handle).expect("capture_screenshot");
    image.save(out_path).unwrap_or_else(|e| panic!("save {out_path}: {e}"));
    println!("wrote {out_path} ({}x{}) view={view}", image.width(), image.height());
}

// Map a view name → an offscreen window. One arm per screen/state you want to shoot.
fn open_root(
    cx: &mut HeadlessAppContext,
    win: Size<Pixels>,
    view: &str,
) -> gpui::WindowHandle<MyRootView> {
    cx.open_window(win, |_, cx| {
        cx.new(|_| match view {
            // "editor" => MyRootView::editor_fixture(),
            _ => MyRootView::default(),
        })
    })
    .expect("open offscreen window")
}
