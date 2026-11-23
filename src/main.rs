#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod es;
mod i18n;
mod profile;
mod task;
mod ui;
mod widgets;

fn main() -> eframe::Result {
    app::timing::start();
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default().with_maximized(true),
        ..Default::default()
    };

    eframe::run_native(
        "Elastic GUI",
        options,
        Box::new(|cc| Ok(Box::new(app::DesktopApp::new(cc)))),
    )
}
