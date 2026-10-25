//! The window: header, navigation, page transitions, and the guard against closing during a write.

use crate::elevation::{self, Privilege};
use crate::icons::Icon;
use crate::logo;
use crate::motion;
use crate::screens::{boot::BootScreen, flash::FlashScreen, format::FormatScreen, home, Screen, Shell};
use crate::theme::{self, *};
use crate::widgets::{dialog, dialog_buttons, heading, Btn};
use crate::worker;
use eframe::egui::{self, pos2, vec2, Align, FontId, Layout, Rect, Sense, Stroke, Ui};

pub struct App {
    last_frame: Option<std::time::Instant>,
    screen: Screen,
    entered: f64,
    flash: FlashScreen,
    boot: BootScreen,
    format: FormatScreen,
    shell: Shell,
    ask_quit: bool,
    quitting: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::install(&cc.egui_ctx);
        // dark title bar and window frame where the system draws them (Windows, macOS, GTK)
        cc.egui_ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(egui::SystemTheme::Dark));
        theme::set_reduce_motion(motion::system_prefers_reduced_motion());
        let screen = match std::env::var("NYFORGE_GUI_START").as_deref() {
            Ok("flash") => Screen::Flash,
            Ok("boot") => Screen::Boot,
            Ok("format") => Screen::Format,
            _ => Screen::Home,
        };
        App {
            last_frame: None,
            screen,
            entered: -10.0,
            flash: FlashScreen::new(),
            boot: BootScreen::new(),
            format: FormatScreen::new(),
            shell: Shell {
                privilege: elevation::privilege(),
                engine_ok: worker::engine_available(),
                missing_prompt_tool: elevation::missing_prompt_tool(),
                go: None,
                restart_error: None,
                wants_restart: false,
            },
            ask_quit: false,
            quitting: false,
        }
    }

    fn busy(&self) -> bool {
        self.flash.busy() || self.boot.busy() || self.format.busy()
    }

    /// Scales the whole interface with the window: a little smaller in small or tiled windows, a little larger on big
    /// screens, unchanged in between. The measure is the window size at zoom 1, so it cannot feed back on itself.
    fn fit_to_window(&mut self, ctx: &egui::Context) {
        let zoom = ctx.zoom_factor();
        let size = ctx.input(|i| i.screen_rect().size()) * zoom;
        if size.x < 50.0 || size.y < 50.0 {
            return;
        }
        let r = (size.x / 860.0).min(size.y / 620.0);
        let want = if r < 1.0 {
            r.max(0.78)
        } else if r > 1.6 {
            (1.0 + (r - 1.6) * 0.5).min(1.3)
        } else {
            1.0
        };
        if (want - zoom).abs() > 0.004 {
            ctx.set_zoom_factor(want);
        }
        // F11: full screen
        if ctx.input(|i| i.key_pressed(egui::Key::F11)) {
            let full = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!full));
        }
    }

    fn abort_all(&mut self) {
        self.flash.abort();
        self.boot.abort();
        self.format.abort();
    }

    fn header(&mut self, ui: &mut Ui) {
        let busy = self.busy();
        let full = ui.max_rect();
        // centre: the mark and the name, like the title of a window
        let title = ui.painter().layout_no_wrap("NyForge".to_string(), FontId::new(15.0, semibold()), TEXT);
        let w = 22.0 + 8.0 + title.size().x;
        let origin = pos2(full.center().x - w / 2.0, full.center().y);
        if self.screen != Screen::Home {
            logo::paint(ui.painter(), Rect::from_center_size(pos2(origin.x + 11.0, origin.y), vec2(22.0, 22.0)), true);
            ui.painter().galley(pos2(origin.x + 30.0, origin.y - title.size().y / 2.0), title, TEXT);
        }
        // left: the way back; right: who is running us
        ui.horizontal_centered(|ui| {
            ui.add_space(12.0);
            if self.screen != Screen::Home {
                let b = Btn::new("Home").small().icon(Icon::Back).enabled(!busy).tip("Finish or cancel the running operation first");
                if b.show(ui).clicked() {
                    self.shell.go = Some(Screen::Home);
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(12.0);
                // the status text gives way to the centred title when the window is narrow
                let narrow = full.width() < 560.0;
                match self.shell.privilege {
                    _ if narrow && self.screen != Screen::Home && !matches!(self.shell.privilege, Privilege::Elevated) => {}
                    _ if narrow && self.screen != Screen::Home => {
                        let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                        crate::icons::paint(ui.painter(), Icon::Shield, r, MUTED);
                    }
                    Privilege::Elevated => {
                        ui.label(small_muted("Administrator"));
                        let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                        crate::icons::paint(ui.painter(), Icon::Shield, r, MUTED);
                    }
                    Privilege::AskPerTask => {
                        ui.label(dim("Asks for permission when writing"));
                    }
                    Privilege::NeedsRestart => {
                        ui.label(dim("Standard user"));
                    }
                }
            });
        });
    }
}

impl eframe::App for App {
    /// Leave at once. Tearing down the graphics context can wait on the compositor, and a window that is slow to close
    /// gets the system's "Application Not Responding" box.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.abort_all();
        std::process::exit(0);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        BG.to_normalized_gamma_f32()
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Without vsync (see main.rs) frames are not throttled by the display, so cap them at about 120 per second.
        if let Some(prev) = self.last_frame {
            let spent = prev.elapsed();
            if spent < std::time::Duration::from_millis(8) {
                std::thread::sleep(std::time::Duration::from_millis(8) - spent);
            }
        }
        self.last_frame = Some(std::time::Instant::now());
        let now = ctx.input(|i| i.time);
        self.fit_to_window(ctx);
        // --- window close: never silently abandon a write
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting {
            if self.busy() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.ask_quit = true;
            }
        }
        if self.quitting && !self.busy() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        // --- files dropped on the window go to NyFlash
        let dropped: Vec<_> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        if let Some(p) = dropped.into_iter().next() {
            if self.screen == Screen::Home || self.screen == Screen::Flash {
                if !self.flash.busy() {
                    self.screen = Screen::Flash;
                    self.entered = now;
                    self.flash.set_image(p, ctx);
                }
            }
        }

        egui::TopBottomPanel::top("header").exact_height(44.0).frame(egui::Frame::new().fill(BAR).stroke(Stroke::new(1.0_f32, with_alpha(BORDER, 110)))).show(ctx, |ui| self.header(ui));

        theme::start_frame(self.screen as u32);
        egui::CentralPanel::default().frame(egui::Frame::new().fill(BG).inner_margin(egui::Margin { left: 20, right: 20, top: 8, bottom: 12 })).show(ctx, |ui| {
            let avail_h = ui.available_height();
            // a soft shadow under the title bar
            {
                let r = ui.max_rect();
                let top = pos2(r.left() - 20.0, r.top() - 8.0);
                let mut m = egui::epaint::Mesh::default();
                let (x0, x1) = (top.x, r.right() + 20.0);
                m.colored_vertex(pos2(x0, top.y), egui::Color32::from_black_alpha(55));
                m.colored_vertex(pos2(x1, top.y), egui::Color32::from_black_alpha(55));
                m.colored_vertex(pos2(x0, top.y + 9.0), egui::Color32::TRANSPARENT);
                m.colored_vertex(pos2(x1, top.y + 9.0), egui::Color32::TRANSPARENT);
                m.add_triangle(0, 1, 3);
                m.add_triangle(0, 3, 2);
                ui.painter().add(egui::Shape::mesh(m));
            }
            // page transition: a short fade and rise; it never blocks input
            let t = if theme::reduce_motion() { 1.0 } else { ((now - self.entered) / 0.18).clamp(0.0, 1.0) as f32 };
            let e = theme::ease_out(t);
            if t < 1.0 {
                ctx.request_repaint();
            }
            // Vertical centring: the page's height from the last frame decides how much room goes above it. The very first
            // frame of a page only measures (invisible), so nothing ever jumps into place.
            let hkey = egui::Id::new(("page-height", self.screen as u32));
            let known: Option<f32> = ctx.data(|d| d.get_temp(hkey));
            ui.set_opacity(if known.is_some() { e } else { 0.0 });
            if known.is_none() {
                ctx.request_repaint();
            }
            let target = known.map_or(0.0, |h| ((avail_h - h) / 2.0).floor().max(0.0));
            // content that changes size glides into place; a window that is being resized follows without lag
            let wkey = egui::Id::new(("page-win", self.screen as u32));
            let last_win: Option<f32> = ctx.data(|d| d.get_temp(wkey));
            ctx.data_mut(|d| d.insert_temp(wkey, avail_h));
            let resizing = last_win.is_some_and(|w| (w - avail_h).abs() > 0.5);
            let pad = ctx.animate_value_with_time(egui::Id::new(("page-pad", self.screen as u32)), target, if theme::reduce_motion() || resizing { 0.0 } else { 0.15 });
            egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt(("page", self.screen as u32)).show(ui, |ui| {
                ui.add_space(pad + (1.0 - e) * 10.0);
                let top = ui.cursor().top();
                let width = ui.available_width().min(780.0);
                // The page is a column of at most 780 px in the middle of the window. (A frame margin cannot do this: it
                // is limited to 127 px a side, which put the page off-centre in windows wider than about 1030 px.)
                let mut area = ui.available_rect_before_wrap();
                area.min.x += ((area.width() - width) / 2.0).floor().max(0.0);
                area.max.x = area.min.x + width;
                ui.scope_builder(egui::UiBuilder::new().max_rect(area), |ui| {
                    ui.set_width(width);
                    match self.screen {
                        Screen::Home => home::ui(ui, &mut self.shell),
                        Screen::Flash => self.flash.ui(ui, &mut self.shell),
                        Screen::Boot => self.boot.ui(ui, &mut self.shell),
                        Screen::Format => self.format.ui(ui, &mut self.shell),
                    }
                });
                let h = ui.cursor().top() - top;
                ctx.data_mut(|d| d.insert_temp(hkey, h));
            });
        });

        // --- navigation requested by a screen or the header
        if let Some(go) = self.shell.go.take() {
            if !self.busy() && go != self.screen {
                self.screen = go;
                self.entered = now;
            }
        }
        // --- keyboard (Ctrl on Windows and Linux, Cmd on macOS), only while no dialog is open and nothing is running:
        //     Ctrl/Cmd+1, 2, 3 open NyFlash, NyBoot, NyFormat; Alt+Left, or Cmd+[ on macOS, goes back to the start screen
        if !self.busy() && ctx.memory(|m| m.top_modal_layer().is_none()) {
            let (cmd, alt) = (egui::Modifiers::COMMAND, egui::Modifiers::ALT);
            let pressed = |m: egui::Modifiers, k: egui::Key| ctx.input_mut(|i| i.consume_key(m, k));
            if pressed(cmd, egui::Key::Num1) {
                self.shell.go = Some(Screen::Flash);
            } else if pressed(cmd, egui::Key::Num2) {
                self.shell.go = Some(Screen::Boot);
            } else if pressed(cmd, egui::Key::Num3) {
                self.shell.go = Some(Screen::Format);
            } else if pressed(alt, egui::Key::ArrowLeft) || pressed(cmd, egui::Key::OpenBracket) {
                self.shell.go = Some(Screen::Home);
            }
            if self.shell.go.is_some() {
                ctx.request_repaint();
            }
        }
        // --- Windows: restart through UAC
        if self.shell.wants_restart {
            self.shell.wants_restart = false;
            match elevation::restart_as_admin() {
                Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                Err(e) => self.shell.restart_error = Some(e),
            }
        }

        // --- closing while a write is running
        if self.ask_quit {
            let (mut stop, mut keep) = (false, false);
            let (_, close) = dialog(ctx, "quit-guard", 420.0, |ui| {
                ui.label(heading("An operation is still running"));
                ui.add_space(6.0);
                ui.label(widgets_muted("Closing NyForge now would stop it halfway and can leave the drive unusable until it is written or formatted again."));
                dialog_buttons(ui, |ui| {
                    if Btn::destructive("Stop and quit").show(ui).clicked() {
                        stop = true;
                    }
                    if Btn::new("Keep going").show(ui).clicked() {
                        keep = true;
                    }
                });
            });
            if stop {
                self.abort_all();
                self.quitting = true;
                self.ask_quit = false;
            } else if keep || close {
                self.ask_quit = false;
            }
        }
    }
}

fn widgets_muted(s: &str) -> egui::RichText {
    theme::muted(s)
}
