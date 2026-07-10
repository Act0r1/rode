#![cfg_attr(not(target_os = "linux"), allow(dead_code, unused_imports))]

mod actions;
mod agent;
mod app;
mod codex;
mod codex_auth;
mod diff;
mod editor;
mod git;
mod notifications;
mod persistence;
mod terminal;
mod theme;
mod ui;

#[cfg(target_os = "linux")]
fn main() {
    use std::path::PathBuf;

    use actions::Quit;
    use app::RodeApp;
    use gpui::{App, AppContext, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size};
    use gpui_platform::application;

    let project_path = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().expect("reading the current directory"));

    application().run(move |cx: &mut App| {
        actions::register(cx);

        let bounds = Bounds::centered(None, size(px(1380.), px(860.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                app_id: Some("dev.rode.Rode".to_owned()),
                titlebar: Some(TitlebarOptions {
                    title: Some("Rode".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            move |window, cx| {
                let project_path = project_path.clone();
                let app = cx.new(|cx| RodeApp::new(project_path, window, cx));
                app.update(cx, |app, cx| app.refresh_codex_account(cx));
                app
            },
        )
        .expect("opening the Rode window");
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.activate(true);
    });
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("Rode currently targets Linux/Wayland only.");
}
