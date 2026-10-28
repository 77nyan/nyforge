//! The window: header, navigation, page transitions, and the guard against closing during a write.

use crate::elevation::{self, Privilege};
use crate::icons::Icon;
use crate::logo;
use crate::motion;
use crate::screens::{boot::BootScreen, flash::FlashScreen, format::FormatScreen, home, nytest::TestScreen, settings as settings_screen, Screen, Shell};
use crate::theme::{self, *};
use crate::widgets::{dialog, dialog_buttons, heading, Btn};
use crate::winstate::WinState;
use crate::worker;
use eframe::egui::{self, pos2, vec2, Align, FontId, Layout, Rect, Sense, Stroke, Ui};

pub struct App {
    last_frame: Option<std::time::Instant>,
    /// Smoothed time one frame takes to build (seconds); paces the frame cap so slow machines are not driven flat out.
    frame_cost: f32,
    screen: Screen,
    entered: f64,
    flash: FlashScreen,
    boot: BootScreen,
    format: FormatScreen,
    test: TestScreen,
    shell: Shell,
    ask_quit: bool,
    quitting: bool,
    /// The window as it will be remembered, and when it last changed (it is saved once it has stopped changing).
    win: WinState,
    win_dirty_since: Option<f64>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, saved: WinState) -> Self {
        theme::install(&cc.egui_ctx);
        // Ctrl/Cmd + and - are handled by NyForge itself (they must work together with the automatic fit to the window)
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
        // dark title bar and window frame where the system draws them (Windows, macOS, GTK)
        cc.egui_ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(egui::SystemTheme::Dark));
        theme::set_reduce_motion(motion::system_prefers_reduced_motion());
        let screen = match std::env::var("NYFORGE_GUI_START").as_deref() {
            Ok("flash") => Screen::Flash,
            Ok("boot") => Screen::Boot,
            Ok("format") => Screen::Format,
            Ok("test") => Screen::Test,
            Ok("settings") => Screen::Settings,
            _ => Screen::Home,
        };
        let settings = crate::settings::Settings::load();
        // NyTest stays out of reach until it is switched on in Settings
        let screen = if screen == Screen::Test && !settings.nytest { Screen::Home } else { screen };
        App {
            last_frame: None,
            frame_cost: 0.002,
            screen,
            entered: -10.0,
            flash: FlashScreen::new(),
            boot: BootScreen::new(),
            format: FormatScreen::new(),
            test: TestScreen::new(),
            shell: Shell {
                privilege: elevation::privilege(),
                engine_ok: worker::engine_available(),
                missing_prompt_tool: elevation::missing_prompt_tool(),
                go: None,
                restart_error: None,
                wants_restart: false,
                anchor_top: false,
                settings,
            },
            ask_quit: false,
            quitting: false,
            win: saved,
            win_dirty_since: None,
        }
    }

    fn busy(&self) -> bool {
        self.flash.busy() || self.boot.busy() || self.format.busy() || self.test.busy()
    }

    /// Scales the whole interface with the window: a little smaller in small or tiled windows, a little larger on big
    /// screens, unchanged in between; on top of that comes the person's own scale (Ctrl/Cmd and + or -, 0 to reset).
    /// The measure is the window size at zoom 1, so it cannot feed back on itself.
    fn fit_to_window(&mut self, ctx: &egui::Context) {
        // --- the person's own scale
        let (cmd, cmd_shift) = (egui::Modifiers::COMMAND, egui::Modifiers::COMMAND | egui::Modifiers::SHIFT);
        let mut step = 0.0_f32;
        let mut reset = false;
        ctx.input_mut(|i| {
            for m in [cmd, cmd_shift] {
                if i.consume_key(m, egui::Key::Plus) || i.consume_key(m, egui::Key::Equals) {
                    step = 0.1;
                }
                if i.consume_key(m, egui::Key::Minus) {
                    step = -0.1;
                }
            }
            if i.consume_key(cmd, egui::Key::Num0) {
                reset = true;
            }
        });
        if reset {
            self.win.scale = 1.0;
        } else if step != 0.0 {
            self.win.scale = ((self.win.scale + step) * 10.0).round() / 10.0;
            self.win.scale = self.win.scale.clamp(0.8, 1.5);
        }
        if reset || step != 0.0 {
            self.win_dirty_since = Some(ctx.input(|i| i.time));
        }

        let zoom = ctx.zoom_factor();
        let size = ctx.input(|i| i.screen_rect().size()) * zoom;
        if size.x < 50.0 || size.y < 50.0 {
            return;
        }
        let r = (size.x / 860.0).min(size.y / 620.0);
        let auto = if r < 1.0 {
            r.max(0.78)
        } else if r > 1.6 {
            (1.0 + (r - 1.6) * 0.5).min(1.3)
        } else {
            1.0
        };
        // never so large that the smallest layout no longer fits the window
        let mut want = (auto * self.win.scale).clamp(0.6, 1.8);
        want = want.min((size.x / 340.0).max(0.6));
        if (want - zoom).abs() > 0.004 {
            ctx.set_zoom_factor(want);
        }
        // F11: full screen (Esc leaves it again)
        let full = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        if ctx.input(|i| i.key_pressed(egui::Key::F11)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!full));
        } else if full && ctx.memory(|m| m.top_modal_layer().is_none()) && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        }
        self.remember_window(ctx, zoom, full);
    }

    /// Notes the window's size so that the next start opens the same way. Written once the size has stopped changing.
    fn remember_window(&mut self, ctx: &egui::Context, zoom: f32, full: bool) {
        let now = ctx.input(|i| i.time);
        let (maxi, inner) = ctx.input(|i| (i.viewport().maximized.unwrap_or(false), i.viewport().inner_rect));
        if !full {
            let mut cur = self.win;
            cur.maximized = maxi;
            // a maximised window keeps the size it had before, so un-maximising returns to it
            if !maxi {
                if let Some(r) = inner {
                    if r.width() > 50.0 && r.height() > 50.0 {
                        cur.width = r.width() * zoom;
                        cur.height = r.height() * zoom;
                    }
                }
            }
            if cur != self.win && (cur.maximized != self.win.maximized || (cur.width - self.win.width).abs() > 0.5 || (cur.height - self.win.height).abs() > 0.5) {
                self.win = cur;
                self.win_dirty_since = Some(now);
            }
        }
        if let Some(t) = self.win_dirty_since {
            if now - t > 1.0 {
                self.win.save();
                self.win_dirty_since = None;
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(1100));
            }
        }
    }

    fn abort_all(&mut self) {
        self.flash.abort();
        self.boot.abort();
        self.format.abort();
        self.test.abort();
    }

    /// The settings cog: only an icon. Hovering makes it a little brighter; while Settings is open it sits in a small quiet disc.
    fn settings_button(&self, ui: &mut Ui, busy: bool) -> egui::Response {
        let (rect, resp) = ui.allocate_exact_size(vec2(26.0, 26.0), if busy { Sense::hover() } else { Sense::click() });
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, !busy, "Settings"));
        let hover = hover_t(ui.ctx(), resp.id.with("h"), !busy && resp.hovered());
        let active = self.screen == Screen::Settings;
        let p = ui.painter();
        if active {
            p.circle_filled(rect.center(), 11.0, FIELD);
        }
        let col = if busy { DIM } else if active { TEXT } else { mix(MUTED, TEXT, hover) };
        crate::icons::gear(p, rect.center(), 6.8, col);
        if resp.has_focus() {
            crate::widgets::focus_ring_r(p, rect, 13.0);
        }
        if !busy && resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        resp
    }

    fn header(&mut self, ui: &mut Ui) {
        let busy = self.busy();
        let full = ui.max_rect();
        // centre: the mark and the name, like the title of a window
        let title = ui.painter().layout_no_wrap("NyForge".to_string(), FontId::new(14.5, semibold()), TEXT);
        let w = 20.0 + 8.0 + title.size().x;
        let origin = pos2(full.center().x - w / 2.0, full.center().y);
        if self.screen != Screen::Home {
            logo::paint(ui.painter(), Rect::from_center_size(pos2(origin.x + 10.0, origin.y), vec2(20.0, 20.0)));
            ui.painter().galley(pos2(origin.x + 28.0, origin.y - title.size().y / 2.0), title, TEXT);
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
                // far right: settings, an icon only
                if self.settings_button(ui, busy).clicked() {
                    self.shell.go = Some(if self.screen == Screen::Settings { Screen::Home } else { Screen::Settings });
                }
                ui.add_space(-2.0);
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
        self.win.save();
        self.abort_all();
        std::process::exit(0);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        BG.to_normalized_gamma_f32()
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // Without vsync (see main.rs) frames are not throttled by the display, so cap them. About 120 per second while
        // frames are cheap (identical to before); when a frame takes long (old CPU, software rendering) the cap eases
        // towards 30 per second so the animation stays smooth without using the whole CPU and starving input and the writer.
        if let Some(cost) = frame.info().cpu_usage {
            self.frame_cost = self.frame_cost * 0.9 + cost.min(0.1) * 0.1;
        }
        let gap = std::time::Duration::from_secs_f32((self.frame_cost * 2.5).clamp(0.008, 0.033));
        if let Some(prev) = self.last_frame {
            let spent = prev.elapsed();
            if spent < gap {
                std::thread::sleep(gap - spent);
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

        egui::TopBottomPanel::top("header").exact_height(36.0).frame(egui::Frame::new().fill(BAR).stroke(Stroke::new(1.0_f32, with_alpha(BORDER, 110)))).show(ctx, |ui| self.header(ui));

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
            let t = if theme::reduce_motion() { 1.0 } else { ((now - self.entered) / 0.30).clamp(0.0, 1.0) as f32 };
            let e = theme::ease_out_soft(t);
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
            let anchored = std::mem::take(&mut self.shell.anchor_top);
            let mut target = if anchored { 14.0 } else { known.map_or(0.0, |h| ((avail_h - h) / 2.0).floor().max(0.0)) };
            // the start screen sits a little above the middle, which reads as better balanced
            if self.screen == Screen::Home {
                target = (target - 26.0).max(0.0);
            }
            // content that changes size glides into place; a window that is being resized follows without lag
            let wkey = egui::Id::new(("page-win", self.screen as u32));
            let last_win: Option<f32> = ctx.data(|d| d.get_temp(wkey));
            ctx.data_mut(|d| d.insert_temp(wkey, avail_h));
            let resizing = last_win.is_some_and(|w| (w - avail_h).abs() > 0.5);
            let pad = ctx.animate_value_with_time(egui::Id::new(("page-pad", self.screen as u32)), target, if theme::reduce_motion() || resizing { 0.0 } else { 0.24 });
            egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt(("page", self.screen as u32)).show(ui, |ui| {
                ui.add_space(pad + (1.0 - e) * 14.0);
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
                        Screen::Test => self.test.ui(ui, &mut self.shell),
                        Screen::Settings => settings_screen::ui(ui, &mut self.shell),
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
        //     Ctrl/Cmd+1 to 4 open NyFlash, NyBoot, NyFormat, NyTest; Alt+Left, or Cmd+[ on macOS, goes back to the start screen
        if !self.busy() && ctx.memory(|m| m.top_modal_layer().is_none()) {
            let (cmd, alt) = (egui::Modifiers::COMMAND, egui::Modifiers::ALT);
            let pressed = |m: egui::Modifiers, k: egui::Key| ctx.input_mut(|i| i.consume_key(m, k));
            if pressed(cmd, egui::Key::Num1) {
                self.shell.go = Some(Screen::Flash);
            } else if pressed(cmd, egui::Key::Num2) {
                self.shell.go = Some(Screen::Boot);
            } else if pressed(cmd, egui::Key::Num3) {
                self.shell.go = Some(Screen::Format);
            } else if pressed(cmd, egui::Key::Num4) && self.shell.settings.nytest {
                self.shell.go = Some(Screen::Test);
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
