//! NyForge: the desktop app for NyFlash, NyBoot and NyFormat.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod drives;
mod elevation;
mod gate;
mod checkdlg;
mod fshelp;
mod icons;
mod logo;
mod motion;
mod run;
mod screens;
mod selector;
mod theme;
mod widgets;
mod worker;

fn main() -> eframe::Result {
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/logo/nyforge-256.png")).ok();
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_title("NyForge")
        .with_app_id("nyforge")
        .with_inner_size([920.0, 680.0])
        // small enough for a quarter of a screen in a tiling window manager
        .with_min_inner_size([360.0, 300.0]);
    if let Some(i) = icon {
        viewport = viewport.with_icon(i);
    }
    // No vsync: with it, swapping buffers waits for the compositor, and a window on another workspace (Hyprland, Sway, GNOME)
    // gets no such signal, so the app stopped answering and the system showed "Application Not Responding".
    let options = eframe::NativeOptions { viewport, vsync: false, ..Default::default() };
    eframe::run_native("NyForge", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
