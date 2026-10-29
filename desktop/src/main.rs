// No console window behind the app in Windows release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod cloud;
mod config;
mod fonts;
mod pdf;
mod storage;
mod sync;

use std::sync::Arc;

use eframe::egui;

fn main() -> eframe::Result {
    let store = match storage::Store::open_default() {
        Ok(store) => Arc::new(store),
        Err(err) => {
            rfd::MessageDialog::new()
                .set_title("freewrite")
                .set_description(format!("Couldn't open the Freewrite folder: {err}"))
                .show();
            std::process::exit(1);
        }
    };

    let icon = eframe::icon_data::from_png_bytes(include_bytes!(
        "../../freewrite/Assets.xcassets/AppIcon.appiconset/256-mac.png"
    ))
    .ok();
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("freewrite")
        .with_app_id("freewrite")
        .with_inner_size([1100.0, 600.0])
        .with_min_inner_size([700.0, 450.0]);
    if let Some(icon) = icon {
        viewport = viewport.with_icon(icon);
    }

    let options = eframe::NativeOptions {
        viewport,
        centered: true,
        ..Default::default()
    };
    eframe::run_native(
        "freewrite",
        options,
        Box::new(|cc| Ok(Box::new(app::FreewriteApp::new(cc, store)))),
    )
}
