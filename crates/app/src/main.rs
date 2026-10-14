use gpui_kit::*;

struct Shell;

impl Render for Shell {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child("Hangar")
    }
}

fn main() {
    gpui_kit::application().run(|cx| {
        gpui_kit::init(cx);
        if let Err(err) =
            gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| cx.new(|_| Shell))
        {
            eprintln!("failed to open window: {err:#}");
            cx.quit();
        }
    });
}
