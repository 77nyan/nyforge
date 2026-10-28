//! The person's choices: the order of the tools on the start screen and whether the optional NyTest is switched on.
//! One small text file in the settings folder (next to the window file); anything unreadable falls back to the defaults.

use crate::screens::Screen;

/// The tools in their default order. NyTest is last, and off until the person turns it on.
pub const TOOLS: [Screen; 4] = [Screen::Flash, Screen::Boot, Screen::Format, Screen::Test];

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// All four tools, in the person's order (NyTest keeps its place here even while it is off).
    pub order: Vec<Screen>,
    pub nytest: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { order: TOOLS.to_vec(), nytest: false }
    }
}

fn key(s: Screen) -> &'static str {
    match s {
        Screen::Flash => "flash",
        Screen::Boot => "boot",
        Screen::Format => "format",
        Screen::Test => "test",
        _ => "",
    }
}

fn from_key(k: &str) -> Option<Screen> {
    TOOLS.iter().copied().find(|s| key(*s) == k.trim())
}

impl Settings {
    /// The tools that are shown on the start screen, in order.
    pub fn visible(&self) -> Vec<Screen> {
        self.order.iter().copied().filter(|s| *s != Screen::Test || self.nytest).collect()
    }

    /// Moves the tool at position `from` of the visible list to position `to`.
    pub fn move_tool(&mut self, from: usize, to: usize) {
        let mut vis = self.visible();
        if from >= vis.len() || to >= vis.len() || from == to {
            return;
        }
        let hidden_at = self.order.iter().position(|s| !vis.contains(s));
        let t = vis.remove(from);
        vis.insert(to, t);
        if let Some(i) = hidden_at {
            vis.insert(i.min(vis.len()), Screen::Test);
        }
        self.order = vis;
        *self = self.clone().sane();
    }

    /// Every tool exactly once; anything missing or repeated is put right.
    pub fn sane(mut self) -> Self {
        let mut out: Vec<Screen> = Vec::new();
        for s in self.order.drain(..) {
            if TOOLS.contains(&s) && !out.contains(&s) {
                out.push(s);
            }
        }
        for s in TOOLS {
            if !out.contains(&s) {
                out.push(s);
            }
        }
        self.order = out;
        self
    }

    pub fn parse(text: &str) -> Settings {
        let mut st = Settings::default();
        let mut order = Vec::new();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            match k.trim() {
                "nytest" => st.nytest = v.trim() == "true",
                "order" => order = v.split(',').filter_map(from_key).collect(),
                _ => {}
            }
        }
        if !order.is_empty() {
            st.order = order;
        }
        st.sane()
    }

    pub fn to_text(&self) -> String {
        let order: Vec<&str> = self.order.iter().map(|s| key(*s)).collect();
        format!("order={}\nnytest={}\n", order.join(","), self.nytest)
    }

    pub fn load() -> Settings {
        crate::winstate::config_file("settings.txt").and_then(|f| std::fs::read_to_string(f).ok()).map(|t| Settings::parse(&t)).unwrap_or_default()
    }

    /// Written at once whenever something changes, so nothing is lost if the app is closed in a hurry.
    pub fn save(&self) {
        let Some(f) = crate::winstate::config_file("settings.txt") else { return };
        if let Some(d) = f.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(f, self.to_text());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_shows_three_tools() {
        let s = Settings::default();
        assert_eq!(s.visible(), vec![Screen::Flash, Screen::Boot, Screen::Format]);
    }

    #[test]
    fn enabling_nytest_shows_four() {
        let s = Settings { nytest: true, ..Default::default() };
        assert_eq!(s.visible().len(), 4);
        assert_eq!(s.visible()[3], Screen::Test);
    }

    #[test]
    fn moving_keeps_the_hidden_tool_in_place() {
        let mut s = Settings::default();
        s.move_tool(0, 2);
        assert_eq!(s.visible(), vec![Screen::Boot, Screen::Format, Screen::Flash]);
        assert_eq!(s.order.len(), 4);
        s.nytest = true;
        assert_eq!(s.visible().len(), 4);
    }

    #[test]
    fn round_trip_and_damage() {
        let mut s = Settings { nytest: true, ..Default::default() };
        s.move_tool(2, 0);
        assert_eq!(Settings::parse(&s.to_text()), s);
        let bad = Settings::parse("order=flash,flash,nonsense\nnytest=maybe\n");
        assert_eq!(bad.order.len(), 4);
        assert!(!bad.nytest);
        assert_eq!(bad.order[0], Screen::Flash);
        assert_eq!(Settings::parse("").order, TOOLS.to_vec());
    }
}
