use gpui::*;
use std::time::Duration;

struct Hello;

impl Render for Hello {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .bg(rgb(0xffffff))
            .size_full()
            .justify_center()
            .items_center()
            .gap_4()
            .child(
                div()
                    .text_color(rgb(0x1c1917))
                    .text_2xl()
                    .child("Hello from GPUI — headless capture"),
            )
            .child(
                div()
                    .px_4()
                    .py_2()
                    .bg(rgb(0x3d63dd))
                    .rounded_md()
                    .text_color(rgb(0xffffff))
                    .child("A rendered button"),
            )
    }
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "/tmp/shot.png".into());
    gpui_platform::application().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(900.), px(640.)), cx);
        let handle = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |_, cx| cx.new(|_| Hello),
            )
            .unwrap();
        cx.activate(true);

        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1200))
                .await;
            let result = cx.update_window(handle.into(), |_, window, _| window.capture_image());
            match result {
                Ok(Ok(img)) => {
                    img.save(&out).expect("save png");
                    println!("CAPTURED {}x{} -> {}", img.width(), img.height(), out);
                }
                Ok(Err(e)) => eprintln!("capture error: {e:?}"),
                Err(e) => eprintln!("update_window error: {e:?}"),
            }
            let _ = cx.update(|cx| cx.quit());
        })
        .detach();
    });
}
