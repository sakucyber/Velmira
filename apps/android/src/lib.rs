#![cfg(target_os = "android")]

#[path = "../../desktop/src/chapter_analysis.rs"]
mod chapter_analysis;
#[path = "../../desktop/src/comic.rs"]
mod comic;
#[path = "../../desktop/src/covers.rs"]
mod covers;
#[path = "../../desktop/src/model_settings.rs"]
mod model_settings;
#[path = "../../desktop/src/opening.rs"]
mod opening;
#[path = "../../desktop/src/pairing_cli.rs"]
mod pairing_cli;
#[path = "../../desktop/src/plugins.rs"]
mod plugins;
#[path = "../../desktop/src/preferences.rs"]
mod preferences;
#[path = "../../desktop/src/reader.rs"]
mod reader;
#[path = "../../desktop/src/reading_preferences.rs"]
mod reading_preferences;
#[path = "../../desktop/src/theme.rs"]
mod theme;
#[path = "../../desktop/src/ui.rs"]
mod ui;
#[path = "../../desktop/src/workers.rs"]
mod workers;

mod automatic_manga;
mod mobile;
mod mobile_ui;
mod models;
mod peer_sync;
mod saf;

#[no_mangle]
fn android_main(app: android_activity::AndroidApp) {
    let Some(data_dir) = app.internal_data_path() else {
        return;
    };
    let options = eframe::NativeOptions {
        android_app: Some(app.clone()),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    if let Err(error) = eframe::run_native(
        "Velmira Native",
        options,
        Box::new(move |cc| {
            Ok(Box::new(mobile::Library::new(
                cc.egui_ctx.clone(),
                app,
                &data_dir,
            )?))
        }),
    ) {
        eprintln!("Velmira Native: {error}");
    }
}
