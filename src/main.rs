mod fmt;
mod rules;
mod scan;
mod trash;
mod treemap;
mod ui;
mod volumes;

use gpui::{
    App, AppContext, Application, Bounds, KeyBinding, Menu, MenuItem, TitlebarOptions, WindowBounds, WindowOptions,
    actions, point, px, size,
};

actions!(devdisk, [Quit]);

fn main() {
    Application::new().run(|cx: &mut App| {
        cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
        cx.set_menus(vec![Menu { name: "DevDisk".into(), items: vec![MenuItem::action("Quit DevDisk", Quit)] }]);

        let bounds = Bounds::centered(None, size(px(1360.), px(860.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("DevDisk".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(16.), px(18.))),
                }),
                window_min_size: Some(size(px(1080.), px(660.))),
                ..Default::default()
            },
            |_, cx| {
                cx.new(|cx| {
                    #[allow(unused_mut)]
                    let mut view = ui::DevDisk::new(cx);
                    #[cfg(debug_assertions)]
                    view.debug_autostart(cx);
                    view
                })
            },
        )
        .expect("failed to open window");
        cx.on_window_closed(|cx| cx.quit()).detach();
        cx.activate(true);
    });
}
