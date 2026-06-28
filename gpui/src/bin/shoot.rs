//! Headless screenshot driver — the agent feedback loop. Seeds a temp vault,
//! opens the editor, and steps through a scripted scenario, capturing a PNG of
//! the *real* rendered GPUI frame after each step (offscreen wgpu readback, no
//! display needed). Run via `shot-driver.sh`; read the PNGs to verify.

use aspgui::{AspApp, Backend};
use gpui::{px, size, App, AppContext, Bounds, WindowBounds, WindowOptions};
use std::time::Duration;

fn seed_vault() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("aspgui-shoot-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("Welcome.md"),
        "# Welcome to your vault\n\nThis is a local-first, end-to-end encrypted notes vault.\n\nEverything you write here syncs peer-to-peer.\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("Ideas.md"),
        "# Ideas\n\n- Build a native GPUI client\n- Time-travel through history\n- Share with a code\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("Notes.md"),
        "# Daily notes\n\nMeeting at 3pm.\nReview the design doc.\n",
    )
    .unwrap();
    dir
}

fn main() {
    let outdir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/aspshots".into());
    std::fs::create_dir_all(&outdir).unwrap();

    let backend = Backend::new().expect("init engine");
    let dir = seed_vault();
    let info = backend.add_local_folder(&dir).expect("add folder");
    let vault_id = info.id.clone();

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

        let outdir = outdir.clone();
        let vault_id = vault_id.clone();
        cx.spawn(async move |cx| {
            let shot = |cx: &mut gpui::AsyncApp, name: &str| {
                let path = format!("{outdir}/{name}.png");
                // Force a synchronous redraw of the current state into
                // `rendered_frame.scene` before capturing — the headless X11 loop
                // does not pump frames on its own under Xvfb.
                match cx.update_window(handle.into(), |_, window, app| {
                    window.draw(app).clear();
                    window.capture_image()
                }) {
                    Ok(Ok(img)) => {
                        img.save(&path).expect("save png");
                        println!("SHOT {name} {}x{}", img.width(), img.height());
                    }
                    Ok(Err(e)) => eprintln!("capture {name}: {e:?}"),
                    Err(e) => eprintln!("window {name}: {e:?}"),
                }
            };
            let settle = |cx: &gpui::AsyncApp| cx.background_executor().timer(Duration::from_millis(450));

            // 1. Connect screen.
            settle(cx).await;
            shot(cx, "01-connect");

            // 2. Open the vault → editor.
            let _ = handle.update(cx, |app, _w, cx| app.open_vault(&vault_id, cx));
            settle(cx).await;
            shot(cx, "02-editor");

            // 3. Select a different file.
            let _ = handle.update(cx, |app, _w, cx| app.select_file("Welcome.md", cx));
            settle(cx).await;
            shot(cx, "03-welcome");

            // 4. Time-travel: jump the playhead to the earliest history event.
            let earliest = handle
                .update(cx, |app, _w, _cx| app.history.iter().map(|e| e.ts).min())
                .ok()
                .flatten();
            if let Some(ts) = earliest {
                let _ = handle.update(cx, |app, _w, cx| app.set_playhead(ts, cx));
            }
            settle(cx).await;
            shot(cx, "04-timetravel");

            // 5a. Edit: enter source-edit mode on Ideas.md and type a new line.
            let _ = handle.update(cx, |app, _w, cx| app.return_to_now(cx));
            let _ = handle.update(cx, |app, _w, cx| app.select_file("Ideas.md", cx));
            let _ = handle.update(cx, |app, window, cx| {
                app.enter_edit(window, cx);
                app.type_str("\n- Edit notes live in GPUI", cx);
            });
            settle(cx).await;
            shot(cx, "08-editing");
            let _ = handle.update(cx, |app, _w, cx| app.exit_edit(cx));
            settle(cx).await;
            shot(cx, "09-edited-rendered");

            // 5. Open the Share modal (generates a code).
            let _ = handle.update(cx, |app, _w, cx| app.open_share(cx));
            settle(cx).await;
            shot(cx, "05-share");
            let _ = handle.update(cx, |app, _w, cx| app.close_share(cx));

            // 6. Toggle dark mode on the editor.
            let _ = handle.update(cx, |app, _w, cx| app.toggle_theme(cx));
            settle(cx).await;
            shot(cx, "06-editor-dark");

            // 7. Back to connect (dark).
            let _ = handle.update(cx, |app, _w, cx| app.back_to_connect(cx));
            settle(cx).await;
            shot(cx, "07-connect-dark");

            let _ = cx.update(|cx| cx.quit());
        })
        .detach();
    });
}
