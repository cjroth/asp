//! Performance + visual harness for the GPUI vault editor — the native analog
//! of the TypeScript app's `web-run.sh` (see ../../perf-harness). It seeds a
//! real on-disk vault at whatever scale you ask for, drives the REAL app, and
//! measures the cost of the thing that actually scales in GPUI: building a
//! frame (`Window::draw` = layout + element paint). It asserts a per-step
//! budget and captures a PNG per step so you can also see what rendered.
//!
//! Run: `cargo run --bin harness --features capture -- [nfiles] [biglines] [nhist] [outdir]`
//! e.g. `cargo run --bin harness --features capture -- 2000 1500 0 /tmp/aspharness`
//!
//! The metric is median `draw()` ms over several frames at a fixed UI state.
//! `draw()` is what re-runs on every keystroke/scroll/selection, so if it is
//! O(file-count) or O(document-length) the app feels slow exactly as reported.

use aspgui::{AspApp, Backend};
use gpui::{px, size, App, AppContext, Bounds, WindowBounds, WindowOptions};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Step {
    name: String,
    median_ms: f64,
    budget_ms: f64,
    ink_ratio: f64, // fraction of non-background pixels (blank-screen / missing-text guard)
}

fn seed_vault(nfiles: usize, biglines: usize) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("aspgui-harness-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for i in 0..nfiles {
        std::fs::write(
            dir.join(format!("note-{i:05}.md")),
            format!("# Note {i}\n\nThis is note number {i} in a large seeded vault.\n"),
        )
        .unwrap();
    }
    if biglines > 0 {
        let mut big = String::from("# Big document\n\n");
        for i in 0..biglines {
            big.push_str(&format!("Line {i}: the quick brown fox jumps over the lazy dog.\n"));
        }
        std::fs::write(dir.join("Big.md"), big).unwrap();
    }
    dir
}

/// Fraction of pixels that differ meaningfully from the top-left (background)
/// pixel — a cheap "is anything actually drawn here" signal.
fn ink_ratio(img: &image::RgbaImage) -> f64 {
    let bg = *img.get_pixel(0, 0);
    let (mut ink, mut total) = (0u64, 0u64);
    // Sample every 4th pixel — plenty for a coverage estimate, much faster.
    for y in (0..img.height()).step_by(4) {
        for x in (0..img.width()).step_by(4) {
            let p = img.get_pixel(x, y);
            let d = (p[0] as i32 - bg[0] as i32).abs()
                + (p[1] as i32 - bg[1] as i32).abs()
                + (p[2] as i32 - bg[2] as i32).abs();
            if d > 24 {
                ink += 1;
            }
            total += 1;
        }
    }
    ink as f64 / total.max(1) as f64
}

fn main() {
    let mut args = std::env::args().skip(1);
    let nfiles: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(2000);
    let biglines: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(1500);
    let _nhist: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let outdir = args.next().unwrap_or_else(|| "/tmp/aspharness".into());
    std::fs::create_dir_all(&outdir).unwrap();

    println!("seeding vault: {nfiles} files, {biglines}-line big file ...");
    let seed_t = Instant::now();
    let backend = Backend::new().expect("init engine");
    let dir = seed_vault(nfiles, biglines);
    let info = backend.add_local_folder(&dir).expect("add folder");
    let vault_id = info.id.clone();
    println!("seeded + indexed in {:.1}s", seed_t.elapsed().as_secs_f32());

    let steps: Arc<Mutex<Vec<Step>>> = Arc::new(Mutex::new(Vec::new()));
    let steps_out = steps.clone();

    gpui_platform::application().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1280.), px(860.)), cx);
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

        let outdir = outdir.clone();
        let vault_id = vault_id.clone();
        let steps = steps.clone();
        cx.spawn(async move |cx| {
            let settle =
                |cx: &gpui::AsyncApp| cx.background_executor().timer(Duration::from_millis(350));

            // Time `draw()` `reps` times at the current state; return median ms
            // and capture the final frame for ink analysis + a saved PNG.
            let measure = |cx: &mut gpui::AsyncApp, name: &str, budget_ms: f64, reps: usize| {
                let mut times = Vec::with_capacity(reps);
                let mut last_img = None;
                for _ in 0..reps {
                    let r = cx.update_window(handle.into(), |_, window, app| {
                        let t0 = Instant::now();
                        window.draw(app).clear();
                        let dt = t0.elapsed().as_secs_f64() * 1000.0;
                        (dt, window.render_to_image().ok())
                    });
                    if let Ok((dt, img)) = r {
                        times.push(dt);
                        last_img = img;
                    }
                }
                times.sort_by(|a, b| a.partial_cmp(b).unwrap());
                let median = times.get(times.len() / 2).copied().unwrap_or(f64::NAN);
                let ink = last_img.as_ref().map(ink_ratio).unwrap_or(0.0);
                if let Some(img) = last_img {
                    let _ = img.save(format!("{outdir}/{name}.png"));
                }
                steps.lock().unwrap().push(Step {
                    name: name.to_string(),
                    median_ms: median,
                    budget_ms,
                    ink_ratio: ink,
                });
                println!("  {name:<22} draw {median:6.1} ms (budget {budget_ms:.0})  ink {ink:.3}");
            };

            settle(cx).await;
            measure(cx, "01-connect", 16.0, 7);

            // Open the big vault (this is where an O(file-count) render shows).
            let _ = handle.update(cx, |app, _w, cx| app.open_vault(&vault_id, cx));
            settle(cx).await;
            measure(cx, "02-editor-open", 16.0, 7);

            // Render a large file read-only.
            let _ = handle.update(cx, |app, _w, cx| app.select_file("Big.md", cx));
            settle(cx).await;
            measure(cx, "03-big-file", 16.0, 7);

            // Enter edit mode + type — the per-keystroke cost.
            let _ = handle.update(cx, |app, window, cx| {
                app.enter_edit(window, cx);
                app.type_str("X", cx);
            });
            settle(cx).await;
            measure(cx, "04-big-file-edit", 16.0, 7);

            println!("\n==== REPORT ====");
            let mut failures = 0;
            for s in steps.lock().unwrap().iter() {
                let slow = s.median_ms > s.budget_ms;
                let blank = s.ink_ratio < 0.002;
                let status = if slow || blank { "FAIL" } else { "ok" };
                if slow || blank {
                    failures += 1;
                }
                let why = match (slow, blank) {
                    (true, true) => " (slow + blank!)",
                    (true, false) => " (over budget)",
                    (false, true) => " (blank/no-ink!)",
                    _ => "",
                };
                println!(
                    "  [{status}] {:<22} {:6.1} ms / {:.0} ms   ink {:.3}{why}",
                    s.name, s.median_ms, s.budget_ms, s.ink_ratio
                );
            }
            println!("{failures} failing step(s). PNGs in {outdir}");

            let _ = cx.update(|cx| cx.quit());
        })
        .detach();
    });

    // Exit non-zero if any step failed its budget (CI-friendly, like web-run.sh).
    let failed = steps_out
        .lock()
        .unwrap()
        .iter()
        .any(|s| s.median_ms > s.budget_ms || s.ink_ratio < 0.002);
    std::process::exit(if failed { 1 } else { 0 });
}
