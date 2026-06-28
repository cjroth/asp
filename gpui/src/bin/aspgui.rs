//! Production entrypoint: open a real window with the vault editor.

use aspgui::{AspApp, Backend};
use gpui::{px, size, App, AppContext, Bounds, WindowBounds, WindowOptions};

fn main() {
    let backend = Backend::new().expect("init engine");
    gpui_platform::application().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1200.), px(820.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|cx| AspApp::new(backend.clone(), cx)),
        )
        .unwrap();
        cx.activate(true);
    });
}
