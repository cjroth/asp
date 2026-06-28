//! Real-input probe harness. Unlike `shoot` (which calls `AspApp` methods
//! directly), this dispatches actual mouse-down/up `PlatformInput` events at
//! pixel coordinates — exercising GPUI hit-testing and `on_click` exactly like
//! a real click. Captures a PNG after each step so we can see what the click
//! actually did. Run: `cargo run --bin probe --features capture`.

use aspgui::{AspApp, Backend};
use gpui::{
    px, size, App, AppContext, Bounds, InputEvent, Modifiers, MouseButton, MouseDownEvent,
    MouseUpEvent, Point, WindowBounds, WindowOptions,
};
use std::time::Duration;

fn seed_vault() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("aspgui-probe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("Welcome.md"),
        "# Welcome to your vault\n\nThis is a local-first, end-to-end encrypted notes vault.\n",
    )
    .unwrap();
    std::fs::write(dir.join("Ideas.md"), "# Ideas\n\n- Build a native GPUI client\n").unwrap();
    dir
}

fn main() {
    let outdir = std::env::args().nth(1).unwrap_or_else(|| "/tmp/aspprobe".into());
    std::fs::create_dir_all(&outdir).unwrap();

    let backend = Backend::new().expect("init engine");
    let dir = seed_vault();
    backend.add_local_folder(&dir).expect("add folder");

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
        cx.spawn(async move |cx| {
            let settle =
                |cx: &gpui::AsyncApp| cx.background_executor().timer(Duration::from_millis(450));

            let shot = |cx: &mut gpui::AsyncApp, name: &str| {
                let path = format!("{outdir}/{name}.png");
                match cx.update_window(handle.into(), |_, window, app| {
                    window.draw(app).clear();
                    window.render_to_image()
                }) {
                    Ok(Ok(img)) => {
                        img.save(&path).unwrap();
                        println!("SHOT {name}");
                    }
                    other => eprintln!("shot {name} failed: {other:?}"),
                }
            };

            // Dispatch a real left click (down+up) at a logical pixel position,
            // after forcing a draw so the hitboxes for the current frame exist.
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

            let screen_name = |cx: &mut gpui::AsyncApp| -> String {
                handle
                    .update(cx, |app, _w, _cx| match app.screen {
                        aspgui::Screen::Connect => "connect".to_string(),
                        aspgui::Screen::Editor => "editor".to_string(),
                    })
                    .unwrap_or_else(|_| "?".into())
            };

            settle(cx).await;
            shot(cx, "1-connect");
            println!("screen before clicks: {}", screen_name(cx));

            // 1) Click the theme toggle (top-right of the card) — validates that
            //    real click simulation works at all.
            click(cx, 813.0, 243.0);
            settle(cx).await;
            shot(cx, "2-after-toggle-click");

            // 2) Click the recent-vault row — should open the editor.
            click(cx, 600.0, 483.0);
            settle(cx).await;
            shot(cx, "3-after-vault-click");
            println!("screen after vault click: {}", screen_name(cx));

            let _ = cx.update(|cx| cx.quit());
        })
        .detach();
    });
}
