pub mod boot;
pub mod flash;
pub mod format;
pub mod home;
pub mod nytest;
pub mod settings;

use crate::elevation::{self, Privilege};
use crate::icons::Icon;
use crate::run::Run;
use crate::theme::*;
use crate::widgets::{self, banner, Btn, Tone};
use eframe::egui::{self, vec2, Id, Ui};
use nyforge_core::format::human_size;
use nyforge_core::progress::Phase;
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Screen {
    Home,
    Flash,
    Boot,
    Format,
    Test,
    Settings,
}

/// What every screen can see about the app.
pub struct Shell {
    pub privilege: Privilege,
    pub engine_ok: bool,
    pub missing_prompt_tool: Option<&'static str>,
    pub go: Option<Screen>,
    pub restart_error: Option<String>,
    pub wants_restart: bool,
    /// A setup page whose height changes with its mode (NyTest) sets this each frame, so the page sits at a fixed
    /// distance from the top instead of being centred: the row of modes then stays where the pointer found it.
    pub anchor_top: bool,
    /// The person's choices (tool order, NyTest on or off); written to disk whenever they change.
    pub settings: crate::settings::Settings,
}

impl Shell {
    /// Can a drive operation be started right now? (Otherwise `why_not` says why, for a tooltip.)
    pub fn can_write(&self) -> bool {
        self.why_not().is_none()
    }

    pub fn why_not(&self) -> Option<String> {
        if !self.engine_ok {
            return Some("The NyForge engine (nyforge) was not found next to this app.".into());
        }
        if self.privilege == Privilege::NeedsRestart {
            return Some("NyForge needs administrator rights to write to drives. Use “Restart as administrator”.".into());
        }
        if let Some(t) = self.missing_prompt_tool {
            return Some(format!("{t} is not installed, so NyForge cannot ask for administrator access."));
        }
        None
    }
}

/// The notice shown at the top of a tool when writing is not possible yet.
pub fn permission_notice(ui: &mut Ui, sh: &mut Shell) {
    if !sh.engine_ok {
        banner(ui, Tone::Error, "NyForge engine not found", "The helper program “nyforge” must sit next to this app. Reinstall NyForge, or set NYFORGE_BIN to its path.", None);
        return;
    }
    if sh.privilege == Privilege::NeedsRestart {
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                banner(ui, Tone::Warn, "Administrator rights needed", "Windows only lets administrators write to drives. You can look around, but nothing can be written until NyForge is restarted as administrator.", None);
            });
        });
        ui.horizontal(|ui| {
            if Btn::primary("Restart as administrator").icon(Icon::Shield).show(ui).clicked() {
                sh.wants_restart = true;
            }
            if let Some(e) = &sh.restart_error {
                ui.label(egui::RichText::new(e).color(WARN).size(12.5));
            }
        });
    }
    if let Some(t) = sh.missing_prompt_tool {
        banner(ui, Tone::Warn, "Cannot ask for administrator access", &format!("{t} is not installed. Install polkit, or start NyForge from a terminal with sudo."), None);
    }
    let _ = elevation::privilege;
}

// ------------------------------------------------------------------ small formatting helpers

pub fn fmt_speed(bps: f64) -> String {
    format!("{}/s", human_size(bps as u64))
}

pub fn fmt_duration(secs: f64) -> String {
    let s = secs.round() as u64;
    if secs < 1.0 {
        "under a second".to_string()
    } else if s < 60 {
        format!("{s} s")
    } else if s < 3600 {
        format!("{} min {:02} s", s / 60, s % 60)
    } else {
        format!("{} h {:02} min", s / 3600, (s % 3600) / 60)
    }
}

pub fn file_name(p: &std::path::Path) -> String {
    p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| p.to_string_lossy().into_owned())
}

/// The engine's own wording for what it is doing, with an ellipsis.
pub fn phase_text(run: &Run) -> String {
    match &run.phase {
        Some((Phase::Preparing, d)) => format!("{}…", d.trim_end_matches('…')),
        Some((Phase::Writing, d)) => format!("{}…", d.trim_end_matches('…')),
        Some((Phase::Finalizing, d)) => format!("{}…", d.trim_end_matches('…')),
        Some((Phase::Verifying, d)) => format!("{}…", d.trim_end_matches('…')),
        None => "Starting…".into(),
    }
}

/// The running-operation panel shared by all the tools: real percentage and bytes when the engine reports them,
/// an indeterminate bar when it does not. Speed and time left only appear once they can be measured.
pub fn run_panel(ui: &mut Ui, id: Id, run: &Run, stopping: bool, rows: &[(&str, String)]) {
    let w = ui.available_width().min(600.0);
    ui.vertical_centered(|ui| ui.allocate_ui(vec2(w, 10.0), |ui| run_panel_inner(ui, id, run, stopping, rows)));
}

fn run_panel_inner(ui: &mut Ui, id: Id, run: &Run, stopping: bool, rows: &[(&str, String)]) {
    widgets::panel(ui, |ui| {
        for (k, v) in rows {
            widgets::kv(ui, k, egui::RichText::new(v.as_str()));
        }
        ui.add_space(10.0);
        let frac = run.fraction();
        ui.horizontal(|ui| {
            match frac {
                Some(f) => ui.label(semibold_text(format!("{:.0}%", f * 100.0), 34.0)),
                None => ui.label(semibold_text("In progress", 20.0)),
            };
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let status = if stopping { "Stopping…".to_string() } else { phase_text(run) };
                ui.label(muted(status));
            });
        });
        widgets::progress_bar(ui, id.with("bar"), frac);
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            if let Some(p) = &run.progress {
                let total = p.bytes_total.or(run.total_hint);
                match total {
                    Some(t) if t > 0 => ui.label(small_muted(format!("{} of {}", human_size(p.bytes_done), human_size(t)))),
                    _ => ui.label(small_muted(human_size(p.bytes_done))),
                };
                let elapsed = run.started.elapsed().as_secs_f64();
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Only once there is enough data for the numbers to mean something.
                    if elapsed > 3.0 && run.verify_started.map_or(true, |t| t.elapsed().as_secs_f64() > 3.0) {
                        if let Some(speed) = run.smooth.speed() {
                            if let Some(eta) = run.smooth.eta().filter(|_| p.fraction > 0.02) {
                                ui.label(small_muted(format!("{} left", fmt_duration(eta))));
                                ui.label(dim("·"));
                            }
                            ui.label(small_muted(fmt_speed(speed)));
                        }
                    }
                });
            }
        });
    });
}

// ------------------------------------------------------------------ file picking

/// The native file picker. For automated testing, NYFORGE_GUI_TEST_PICK="/a.iso:/b.iso" answers it instead.
pub fn pick_files(title: &str, filters: &[(&str, &[&str])], multiple: bool) -> Vec<PathBuf> {
    if let Some(v) = std::env::var_os("NYFORGE_GUI_TEST_PICK") {
        let all: Vec<PathBuf> = std::env::split_paths(&v).collect();
        return if multiple { all } else { all.into_iter().take(1).collect() };
    }
    let mut d = rfd::FileDialog::new().set_title(title);
    for (name, exts) in filters {
        d = d.add_filter(*name, exts);
    }
    if multiple {
        d.pick_files().unwrap_or_default()
    } else {
        d.pick_file().into_iter().collect()
    }
}

/// The native "save as" dialog. For automated testing, NYFORGE_GUI_TEST_SAVE="/a.img" answers it instead.
pub fn pick_save(title: &str, file_name: &str) -> Option<PathBuf> {
    if let Some(v) = std::env::var_os("NYFORGE_GUI_TEST_SAVE") {
        return Some(PathBuf::from(v));
    }
    rfd::FileDialog::new().set_title(title).set_file_name(file_name).save_file()
}

