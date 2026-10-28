//! Remembers the window between runs: its size, whether it was maximised, and the interface scale the person chose
//! (Ctrl/Cmd and + or -). One small text file in the user's settings folder; anything unreadable is simply ignored.

use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WinState {
    /// Inner size in logical points (what the window manager sees).
    pub width: f32,
    pub height: f32,
    pub maximized: bool,
    /// The person's own scale factor, 0.8 to 1.5, on top of the automatic fit to the window.
    pub scale: f32,
}

pub const MIN_SIZE: [f32; 2] = [360.0, 300.0];
pub const DEFAULT_SIZE: [f32; 2] = [920.0, 680.0];

impl Default for WinState {
    fn default() -> Self {
        WinState { width: DEFAULT_SIZE[0], height: DEFAULT_SIZE[1], maximized: false, scale: 1.0 }
    }
}

/// A file in NyForge's settings folder.
pub fn config_file(name: &str) -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)?
    } else if cfg!(target_os = "macos") {
        PathBuf::from(std::env::var_os("HOME")?).join("Library/Application Support")
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).filter(|p| p.is_absolute()).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?
    };
    Some(base.join("nyforge").join(name))
}

fn file() -> Option<PathBuf> {
    config_file("window.txt")
}

fn parse(text: &str) -> Option<WinState> {
    let mut st = WinState::default();
    for line in text.lines() {
        let (k, v) = line.split_once('=')?;
        match k.trim() {
            "width" => st.width = v.trim().parse().ok()?,
            "height" => st.height = v.trim().parse().ok()?,
            "maximized" => st.maximized = v.trim() == "true",
            "scale" => st.scale = v.trim().parse().ok()?,
            _ => {}
        }
    }
    Some(st.sane())
}

impl WinState {
    /// Keeps values inside what makes sense, so a damaged file or a vanished monitor cannot open an unusable window.
    pub fn sane(mut self) -> Self {
        let ok = |v: f32| v.is_finite();
        if !ok(self.width) || !ok(self.height) || !ok(self.scale) {
            return WinState::default();
        }
        self.width = self.width.clamp(MIN_SIZE[0], 3200.0);
        self.height = self.height.clamp(MIN_SIZE[1], 2200.0);
        self.scale = self.scale.clamp(0.8, 1.5);
        self
    }

    pub fn load() -> WinState {
        file().and_then(|f| std::fs::read_to_string(f).ok()).and_then(|t| parse(&t)).unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(f) = file() else { return };
        if let Some(d) = f.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let s = self.sane();
        let _ = std::fs::write(f, format!("width={:.0}\nheight={:.0}\nmaximized={}\nscale={:.2}\n", s.width, s.height, s.maximized, s.scale));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_state_reads_back() {
        let s = parse("width=1000\nheight=700\nmaximized=true\nscale=1.2\n").unwrap();
        assert_eq!((s.width, s.height, s.maximized), (1000.0, 700.0, true));
        assert!((s.scale - 1.2).abs() < 1e-6);
    }

    #[test]
    fn nonsense_cannot_make_an_unusable_window() {
        let s = parse("width=3\nheight=99999\nmaximized=false\nscale=9\n").unwrap();
        assert_eq!(s.width, MIN_SIZE[0]);
        assert_eq!(s.height, 2200.0);
        assert_eq!(s.scale, 1.5);
        assert!(parse("width=abc\n").is_none());
        assert_eq!(WinState { width: f32::NAN, ..Default::default() }.sane(), WinState::default());
    }
}
