use std::sync::Arc;

use gpui::{
    div, prelude::*, px, rgb, size, AnyWindowHandle, App, Bounds, Context, HeadlessAppContext,
    Pixels, Size, WindowBounds, WindowOptions,
};
use gpui_platform::application;

mod app;
mod assets;
mod engine;
mod icons;
mod perf;
mod screens;
mod theme;
mod vault;

use app::AspApp;
use assets::Assets;
use theme::Theme;

struct HelloWorld;

impl Render for HelloWorld {
    fn render(&mut self, _window: &mut gpui::Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .size_full()
            .bg(rgb(0x0d0d0f))
            .text_color(rgb(0xe6e6e6))
            .justify_center()
            .items_center()
            .child("ASP — gpui port")
    }
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // Offscreen screenshot mode: `asp-gpui --shot <out.png> [screen]` renders a
    // screen to a PNG via the headless wgpu renderer (no real window/display).
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--perf") {
        run_perf();
        return;
    }
    if let Some(idx) = args.iter().position(|a| a == "--shot") {
        let out = args
            .get(idx + 1)
            .cloned()
            .unwrap_or_else(|| "shot.png".to_string());
        let screen = args.get(idx + 2).cloned().unwrap_or_else(|| "connect".into());
        run_shot(&out, &screen);
        return;
    }

    application().with_assets(Assets).run(|cx: &mut App| {
        cx.text_system().add_fonts(Assets::font_bytes()).ok();
        let bounds = Bounds::centered(None, size(px(1100.0), px(740.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|_| AspApp::new()),
        )
        .unwrap();
        cx.activate(true);
    });
}

/// Performance report — logic hot paths + offscreen render frames, at real scale.
fn run_perf() {
    use std::time::Instant;
    println!("== asp-gpui perf (build profile matters — run with --release for real numbers) ==");
    let (parse_ms, tree_ms) = perf::time_logic(8000, 2000, 30);
    println!("markdown parse        (8k lines):        {parse_ms:7.2} ms/iter");
    println!("tree build+flatten    (2k files):        {tree_ms:7.2} ms/iter");

    let text_system = Arc::new(gpui_wgpu::CosmicTextSystem::new("sans-serif"));
    let mut cx = HeadlessAppContext::with_platform(
        text_system,
        Arc::new(Assets),
        gpui_platform::current_headless_renderer,
    );
    cx.text_system().add_fonts(Assets::font_bytes()).ok();
    let window = cx
        .open_window(size(px(1100.0), px(740.0)), |_, cx| {
            cx.new(|_| {
                let mut a = AspApp::fixture_editor(Theme::light());
                a.content = perf::big_doc(1000);
                a.files = perf::many_files(300);
                a.expanded = a
                    .files
                    .iter()
                    .filter(|(_, d)| *d)
                    .map(|(p, _)| (p.clone(), true))
                    .collect();
                a
            })
        })
        .expect("open window");
    let handle: AnyWindowHandle = window.into();
    cx.run_until_parked();
    let _ = cx.capture_screenshot(handle); // warmup

    let frames = 15;
    let t = Instant::now();
    for _ in 0..frames {
        cx.update_window(handle, |_, window, _| window.refresh()).ok();
        cx.run_until_parked();
        let _ = cx.capture_screenshot(handle);
    }
    let frame_ms = t.elapsed().as_secs_f64() * 1000.0 / frames as f64;
    println!("editor render_to_image (1k-line doc, 300 files): {frame_ms:7.2} ms/frame (software lavapipe)");
    println!("note: render uses CPU lavapipe; real GPU is far faster. CPU-side scene build is the comparable metric.");
}

/// Renders the named screen offscreen and writes it to `out_path`.
fn run_shot(out_path: &str, screen: &str) {
    let text_system = Arc::new(gpui_wgpu::CosmicTextSystem::new("sans-serif"));
    let mut cx = HeadlessAppContext::with_platform(
        text_system,
        Arc::new(Assets),
        gpui_platform::current_headless_renderer,
    );
    cx.text_system().add_fonts(Assets::font_bytes()).ok();

    let win_size: Size<Pixels> = size(px(1100.0), px(740.0));
    let handle: AnyWindowHandle = match screen {
        "hello" => cx
            .open_window(win_size, |_, cx| cx.new(|_| HelloWorld))
            .expect("open window")
            .into(),
        "connect-dark" => cx
            .open_window(win_size, |_, cx| cx.new(|_| AspApp::fixture_connect(Theme::dark())))
            .expect("open window")
            .into(),
        "share-modal" => cx
            .open_window(win_size, |_, cx| {
                cx.new(|_| {
                    let mut a = AspApp::fixture_connect(Theme::light());
                    a.modal = app::Modal::ShareVault {
                        name: "Research Notes".into(),
                        ticket: Some("asp1qyqszqgpqyqszqgpqyqszqgpqyqszqgp-key-9f2a".into()),
                    };
                    a
                })
            })
            .expect("open window")
            .into(),
        "connect-modal" => cx
            .open_window(win_size, |_, cx| {
                cx.new(|_| {
                    let mut a = AspApp::fixture_connect(Theme::light());
                    a.open_connect();
                    a.connect_type("asp1qyqszqgpqyqszqgpqyqszqgp");
                    a
                })
            })
            .expect("open window")
            .into(),
        "remove-modal" => cx
            .open_window(win_size, |_, cx| {
                cx.new(|_| {
                    let mut a = AspApp::fixture_connect(Theme::light());
                    a.open_remove("v1", "Research Notes");
                    a
                })
            })
            .expect("open window")
            .into(),
        "editor" => cx
            .open_window(win_size, |_, cx| cx.new(|_| AspApp::fixture_editor(Theme::light())))
            .expect("open window")
            .into(),
        "editor-dark" => cx
            .open_window(win_size, |_, cx| cx.new(|_| AspApp::fixture_editor(Theme::dark())))
            .expect("open window")
            .into(),
        "editor-edit" => cx
            .open_window(win_size, |_, cx| {
                cx.new(|_| {
                    let mut a = AspApp::fixture_editor(Theme::light());
                    a.begin_edit();
                    a
                })
            })
            .expect("open window")
            .into(),
        _ => cx
            .open_window(win_size, |_, cx| cx.new(|_| AspApp::fixture_connect(Theme::light())))
            .expect("open window")
            .into(),
    };

    // Drive layout + an initial draw, then refresh to guarantee a fresh frame.
    cx.run_until_parked();
    cx.update_window(handle, |_, window, _| window.refresh())
        .expect("refresh window");
    cx.run_until_parked();

    let image = cx.capture_screenshot(handle).expect("capture screenshot");
    image
        .save(out_path)
        .unwrap_or_else(|e| panic!("failed to save PNG to {out_path}: {e}"));
    println!(
        "wrote {out_path} ({}x{}) screen={screen}",
        image.width(),
        image.height()
    );
}
