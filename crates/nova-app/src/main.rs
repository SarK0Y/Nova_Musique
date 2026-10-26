//! Nova — a local music library player with an Apollo-flavoured UI.
#[allow(unused)]
mod app;
mod mpv;
mod videowin;

fn main() -> eframe::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // Re-launched by the main window to host the video-stream picker in its own
    // process, so that its GL context cannot collide with mpv's.
    if std::env::args_os().any(|arg| arg == videowin::FLAG) {
        return videowin::run();
    }

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 780.0])
            .with_min_inner_size([880.0, 560.0])
            .with_drag_and_drop(true)
            .with_title("Nova Musique"),
        ..Default::default()
    };

    eframe::run_native(
        "Nova Musique",
        options,
        Box::new(|cc| Ok(Box::new(app::NovaApp::new(cc)))),
    )
}
