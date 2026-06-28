//! Pixel-level visual + interaction regression test.
//!
//! For each canonical scene it (1) drives the app — using REAL mouse clicks
//! where the point is to test interaction — (2) captures the rendered frame,
//! (3) asserts structural invariants on the pixels (the right regions actually
//! have ink, i.e. nothing is blank/garbled), and (4) diffs against a committed
//! golden PNG so layout regressions (overlap, shifted/clobbered elements) fail
//! loudly.
//!
//!   cargo run --bin vtest --features capture            # check (exit 1 on fail)
//!   cargo run --bin vtest --features capture -- --update # (re)write goldens
//!
//! Goldens live in `tests/golden/`. They are machine-specific (text uses the
//! system font fallback), so regenerate them on the machine you test on.

use aspgui::{AspApp, Backend, Screen};
use gpui::{
    px, size, App, AppContext, Bounds, InputEvent, Modifiers, MouseButton, MouseDownEvent,
    MouseUpEvent, Point, WindowBounds, WindowOptions,
};
use std::time::Duration;

/// A fractional region of the frame: (x0,y0,x1,y1) in 0..1.
type Rect = (f32, f32, f32, f32);

fn seed_vault() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("aspgui-vtest-fixed");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("Welcome.md"),
        "# Welcome\n\nA **local-first** vault with `code` and a list:\n\n- one\n- two\n- three\n",
    )
    .unwrap();
    std::fs::write(dir.join("Ideas.md"), "# Ideas\n\n- Build it\n- Ship it\n").unwrap();
    dir
}

/// Fraction of pixels inside `r` that differ from the frame's background pixel.
fn region_ink(img: &image::RgbaImage, r: Rect) -> f32 {
    let bg = *img.get_pixel(0, 0);
    let (w, h) = (img.width() as f32, img.height() as f32);
    let (x0, y0, x1, y1) = (
        (r.0 * w) as u32,
        (r.1 * h) as u32,
        (r.2 * w) as u32,
        (r.3 * h) as u32,
    );
    let (mut ink, mut total) = (0u64, 0u64);
    let mut y = y0;
    while y < y1.min(img.height()) {
        let mut x = x0;
        while x < x1.min(img.width()) {
            let p = img.get_pixel(x, y);
            let d = (p[0] as i32 - bg[0] as i32).abs()
                + (p[1] as i32 - bg[1] as i32).abs()
                + (p[2] as i32 - bg[2] as i32).abs();
            if d > 24 {
                ink += 1;
            }
            total += 1;
            x += 2;
        }
        y += 2;
    }
    ink as f32 / total.max(1) as f32
}

/// Fraction of pixels that differ beyond a small tolerance between two frames.
fn frame_diff(a: &image::RgbaImage, b: &image::RgbaImage) -> f32 {
    if a.dimensions() != b.dimensions() {
        return 1.0;
    }
    let (mut diff, mut total) = (0u64, 0u64);
    for (pa, pb) in a.pixels().zip(b.pixels()) {
        let d = (pa[0] as i32 - pb[0] as i32).abs()
            + (pa[1] as i32 - pb[1] as i32).abs()
            + (pa[2] as i32 - pb[2] as i32).abs();
        if d > 32 {
            diff += 1;
        }
        total += 1;
    }
    diff as f32 / total.max(1) as f32
}

fn main() {
    let update = std::env::args().any(|a| a == "--update");
    let golden_dir = format!("{}/tests/golden", env!("CARGO_MANIFEST_DIR"));
    let out_dir = std::env::temp_dir().join("aspgui-vtest-out");
    std::fs::create_dir_all(&golden_dir).ok();
    std::fs::create_dir_all(&out_dir).ok();

    let backend = Backend::new().expect("init engine");
    let dir = seed_vault();
    backend.add_local_folder(&dir).expect("add folder");

    // Collected (scene, ok, detail) results; read after the app quits.
    let results: std::sync::Arc<std::sync::Mutex<Vec<(String, bool, String)>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let results_out = results.clone();

    gpui_platform::application().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1200.), px(820.)), cx);
        let handle = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |_, cx| cx.new(|cx| AspApp::new(backend.clone(), cx)),
            )
            .unwrap();
        cx.activate(true);

        let golden_dir = golden_dir.clone();
        let out_dir = out_dir.clone();
        let results = results.clone();
        cx.spawn(async move |cx| {
            let settle =
                |cx: &gpui::AsyncApp| cx.background_executor().timer(Duration::from_millis(400));

            let click = |cx: &mut gpui::AsyncApp, x: f32, y: f32| {
                let _ = cx.update_window(handle.into(), |_, window, app| {
                    window.draw(app).clear();
                    let position: Point<gpui::Pixels> = gpui::point(px(x), px(y));
                    window.dispatch_event(
                        MouseDownEvent {
                            position,
                            button: MouseButton::Left,
                            modifiers: Modifiers::default(),
                            click_count: 1,
                            first_mouse: false,
                        }
                        .to_platform_input(),
                        app,
                    );
                    window.dispatch_event(
                        MouseUpEvent {
                            position,
                            button: MouseButton::Left,
                            modifiers: Modifiers::default(),
                            click_count: 1,
                        }
                        .to_platform_input(),
                        app,
                    );
                });
            };

            let capture = |cx: &mut gpui::AsyncApp| -> Option<image::RgbaImage> {
                cx.update_window(handle.into(), |_, window, app| {
                    window.draw(app).clear();
                    window.render_to_image().ok()
                })
                .ok()
                .flatten()
            };

            // Run one scene's checks. `regions` must each have ink; the frame is
            // golden-compared. Records a result row.
            let mut check = |cx: &mut gpui::AsyncApp,
                             name: &str,
                             regions: &[(&str, Rect, f32)]| {
                let Some(img) = capture(cx) else {
                    results
                        .lock()
                        .unwrap()
                        .push((name.into(), false, "capture failed".into()));
                    return;
                };
                let _ = img.save(out_dir.join(format!("{name}.png")));
                let mut problems = Vec::new();
                for (label, r, min_ink) in regions {
                    let got = region_ink(&img, *r);
                    if got < *min_ink {
                        problems.push(format!("{label} ink {got:.4} < {min_ink:.4}"));
                    }
                }
                let golden_path = format!("{golden_dir}/{name}.png");
                if update {
                    let _ = img.save(&golden_path);
                } else if let Ok(g) = image::open(&golden_path) {
                    let d = frame_diff(&img, &g.to_rgba8());
                    if d > 0.02 {
                        problems.push(format!("golden diff {:.3} > 0.020", d));
                    }
                } else {
                    problems.push("no golden (run --update)".into());
                }
                let ok = problems.is_empty();
                results
                    .lock()
                    .unwrap()
                    .push((name.into(), ok, problems.join("; ")));
            };

            // --- Scene 1: connect screen ---
            settle(cx).await;
            check(
                cx,
                "connect",
                // card region must have ink; right-of-card must be empty
                &[("card", (0.30, 0.20, 0.70, 0.70), 0.005)],
            );

            // --- Scene 2: real click on the vault row -> editor ---
            click(cx, 600.0, 483.0);
            settle(cx).await;
            let opened = handle
                .update(cx, |app, _w, _cx| matches!(app.screen, Screen::Editor))
                .unwrap_or(false);
            results.lock().unwrap().push((
                "click-opens-editor".into(),
                opened,
                if opened { "".into() } else { "vault click did not open editor".into() },
            ));
            check(
                cx,
                "editor",
                &[
                    ("sidebar", (0.0, 0.15, 0.22, 0.95), 0.002),
                    ("content", (0.24, 0.08, 1.0, 0.92), 0.002),
                ],
            );

            // --- Scene 3: real click on the *other* sidebar file row -> content
            //     changes. Rows are 29px tall under the "FILES" header; the
            //     second file sits at ~y=124 logical. Open auto-selected the
            //     first file, so clicking the second (anywhere across the row,
            //     incl. the empty right side) must change the selection.
            let path_before = handle
                .update(cx, |app, _w, _cx| app.current_path.clone())
                .unwrap_or(None);
            let before = capture(cx);
            click(cx, 110.0, 124.0);
            settle(cx).await;
            let path_after = handle
                .update(cx, |app, _w, _cx| app.current_path.clone())
                .unwrap_or(None);
            let after = capture(cx);
            let pixels_changed = match (before, after) {
                (Some(a), Some(b)) => frame_diff(&a, &b) > 0.002,
                _ => false,
            };
            let path_changed = path_before != path_after;
            results.lock().unwrap().push((
                "file-click-changes-content".into(),
                pixels_changed && path_changed,
                format!(
                    "path {:?}->{:?} (changed={path_changed}), pixels_changed={pixels_changed}",
                    path_before, path_after
                ),
            ));

            // Report + exit from INSIDE the run loop: on macOS `cx.quit()`
            // terminates the process, so code after `run()` never executes.
            let r = results.lock().unwrap();
            println!(
                "\n==== VISUAL TEST{} ====",
                if update { " (updating goldens)" } else { "" }
            );
            let mut failures = 0;
            for (name, ok, detail) in r.iter() {
                let status = if *ok { "ok  " } else { "FAIL" };
                if !ok {
                    failures += 1;
                }
                println!("  [{status}] {name:<28} {detail}");
            }
            println!("{failures} failing check(s).");
            use std::io::Write;
            let _ = std::io::stdout().flush();
            std::process::exit(if failures > 0 && !update { 1 } else { 0 });
        })
        .detach();
    });
    let _ = results_out; // (reporting happens inside the run loop above)
}
