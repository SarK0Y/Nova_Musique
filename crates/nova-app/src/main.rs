//! Nova — a local music library player with an Apollo-flavoured UI.

mod app;
mod mpv;

fn main() -> eframe::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

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
