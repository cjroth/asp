use gpui::{
    div, prelude::*, px, rgb, size, App, Bounds, Context, WindowBounds, WindowOptions,
};
use gpui_platform::application;

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
    application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1100.0), px(740.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|_| HelloWorld),
        )
        .unwrap();
        cx.activate(true);
    });
}
