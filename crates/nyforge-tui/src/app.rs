//! State and behaviour of the terminal app: what each tool asks for, and what happens on a key press.

use crate::drives::{self, Candidate, Purpose};
use crate::engine::{self, Outcome, Run};
use nyforge_core::format::{human_duration, human_size, human_speed};
use nyforge_core::progress::Phase;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Flash,
    Boot,
    Format,
    Test,
}

pub const TOOLS: [(Tool, &str); 4] = [(Tool::Flash, "NyFlash"), (Tool::Boot, "NyBoot"), (Tool::Format, "NyFormat"), (Tool::Test, "NyTest")];

pub enum Kind {
    Text(String),
    Pick(Vec<&'static str>, usize),
    Toggle(bool),
    Drive,
}

pub struct Field {
    pub id: &'static str,
    pub label: &'static str,
    pub kind: Kind,
    pub hint: &'static str,
    /// Shown only while the field `.0` is set to `.1`.
    pub when: Option<(&'static str, &'static str)>,
}

fn f(id: &'static str, label: &'static str, kind: Kind, hint: &'static str) -> Field {
    Field { id, label, kind, hint, when: None }
}

fn pick(opts: &[&'static str]) -> Kind {
    Kind::Pick(opts.to_vec(), 0)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Special {
    None,
    BootLs,
    Test,
}

pub struct Pending {
    args: Vec<String>,
    cand: Option<Candidate>,
    purpose: Purpose,
    destructive: bool,
    needs_finished: bool,
    special: Special,
    rows: Vec<(&'static str, String)>,
    verb: &'static str,
}

pub struct Done {
    pub ok: Option<bool>, // None = cancelled
    pub title: String,
    pub lines: Vec<String>,
}

pub enum Mode {
    Form,
    Confirm(Box<Pending>),
    Running,
    Done(Done),
}

pub struct Page {
    pub tool: Tool,
    pub fields: Vec<Field>,
    pub focus: usize,
    pub editing: bool,
    pub drives: Vec<Candidate>,
    pub drive_sel: Option<usize>,
    pub drive_err: Option<String>,
    pub mode: Mode,
    pub run: Option<Run>,
    pub special: bool,
    pub msg: Option<String>,
    pub scroll: u16,
    pub verb: &'static str,
    purpose: Purpose,
}

impl Page {
    fn new(tool: Tool) -> Page {
        let (fields, purpose) = match tool {
            Tool::Flash => (
                vec![
                    f("image", "Image", Kind::Text(String::new()), "path to an .iso or .img (also .gz .xz .zst .zip) or a URL"),
                    f("drive", "Drive", Kind::Drive, ""),
                    f("verify", "Verify", Kind::Toggle(true), "read the drive back afterwards"),
                    f("eject", "Eject", Kind::Toggle(false), "detach the drive when done"),
                ],
                Purpose::Flash,
            ),
            Tool::Boot => (
                vec![
                    f("action", "Action", pick(&["Install", "Update", "Add ISO", "Remove", "List"]), ""),
                    f("drive", "Drive", Kind::Drive, ""),
                    Field { when: Some(("action", "Install")), ..f("label", "Label", Kind::Text("NYBOOT".into()), "") },
                    Field { when: Some(("action", "Add ISO")), ..f("file", "ISO", Kind::Text(String::new()), "path to an .iso (or several, separated by a space)") },
                    Field { when: Some(("action", "Remove")), ..f("item", "Item", Kind::Text(String::new()), "path on the drive, shown by List") },
                ],
                Purpose::Flash,
            ),
            Tool::Format => (
                vec![
                    f("drive", "Drive", Kind::Drive, ""),
                    f("fs", "Filesystem", pick(&["Auto", "FAT32", "exFAT", "NTFS", "ext4"]), ""),
                    f("label", "Label", Kind::Text(String::new()), "optional"),
                    f("scheme", "Partitions", pick(&["Auto", "MBR", "GPT", "None"]), ""),
                    f("wipe", "Erase", pick(&["Quick", "Zero", "Random", "Discard"]), ""),
                    f("cap", "Capacity test", pick(&["Off", "Quick", "Full"]), ""),
                ],
                Purpose::Format,
            ),
            Tool::Test => (
                vec![
                    f("drive", "Drive", Kind::Drive, ""),
                    f("preset", "Depth", Kind::Pick(vec!["Quick", "Standard", "Full"], 1), ""),
                    f("erase", "Erase", Kind::Toggle(false), "also test writing straight to the drive"),
                ],
                Purpose::Test,
            ),
        };
        let mut p = Page { tool, fields, focus: 0, editing: false, drives: vec![], drive_sel: None, drive_err: None, mode: Mode::Form, run: None, special: false, msg: None, scroll: 0, verb: "", purpose };
        p.refresh();
        p.verb = p.start_label();
        p
    }

    pub fn start_label(&self) -> &'static str {
        match self.tool {
            Tool::Flash => "Flash",
            Tool::Boot => match self.pick_val("action") {
                "Install" => "Install",
                "Update" => "Update",
                "Add ISO" => "Add",
                "Remove" => "Remove",
                _ => "List",
            },
            Tool::Format => "Format",
            Tool::Test => "Start",
        }
    }

    pub fn has_drive(&self) -> bool {
        self.fields.iter().any(|f| matches!(f.kind, Kind::Drive))
    }

    pub fn refresh(&mut self) {
        if !self.has_drive() {
            return;
        }
        let keep = self.drive_sel.and_then(|i| self.drives.get(i)).map(|c| c.drive.path.clone());
        match drives::scan(self.purpose) {
            Ok(d) => {
                self.drives = d;
                self.drive_err = None;
            }
            Err(e) => {
                self.drives.clear();
                self.drive_err = Some(e);
            }
        }
        self.drive_sel = keep.and_then(|p| self.drives.iter().position(|c| c.drive.path == p && c.blocked.is_none()));
        if self.drive_sel.is_none() {
            let ok: Vec<usize> = (0..self.drives.len()).filter(|&i| self.drives[i].blocked.is_none()).collect();
            if ok.len() == 1 {
                self.drive_sel = Some(ok[0]);
            }
        }
    }

    pub fn pick_val(&self, id: &str) -> &'static str {
        for f in &self.fields {
            if f.id == id {
                if let Kind::Pick(o, i) = &f.kind {
                    return o[*i];
                }
            }
        }
        ""
    }
    fn text(&self, id: &str) -> String {
        for f in &self.fields {
            if f.id == id {
                if let Kind::Text(s) = &f.kind {
                    return s.trim().to_string();
                }
            }
        }
        String::new()
    }
    fn on(&self, id: &str) -> bool {
        self.fields.iter().any(|f| f.id == id && matches!(f.kind, Kind::Toggle(true)))
    }

    /// Indices of the fields that apply right now.
    pub fn visible(&self) -> Vec<usize> {
        (0..self.fields.len()).filter(|&i| self.fields[i].when.is_none_or(|(id, v)| self.pick_val(id) == v)).collect()
    }

    fn rows(&self) -> usize {
        self.visible().len() + 1 // + the start button
    }

    fn cur(&self) -> Option<usize> {
        self.visible().get(self.focus).copied()
    }

    fn text_edit(&mut self, f: impl FnOnce(&mut String)) {
        if let Some(i) = self.cur() {
            if let Kind::Text(s) = &mut self.fields[i].kind {
                f(s);
            }
        }
    }

    fn selected(&self) -> Result<&Candidate, String> {
        self.drive_sel.and_then(|i| self.drives.get(i)).ok_or_else(|| "Choose a drive first (← → on the Drive row, r to look again).".to_string())
    }

    fn build(&self) -> Result<Pending, String> {
        let mut args: Vec<String> = vec![];
        let mut rows = vec![];
        let mut destructive = false;
        let mut special = Special::None;
        let mut needs_finished = false;
        let cand = if self.has_drive() { Some(self.selected()?.clone()) } else { None };
        let dpath = cand.as_ref().map(|c| c.drive.path.clone()).unwrap_or_default();
        match self.tool {
            Tool::Flash => {
                let img = self.text("image");
                if img.is_empty() {
                    return Err("Enter the path of an image.".into());
                }
                let img = if img.starts_with("http://") || img.starts_with("https://") { img } else { expand(&img) };
                if !img.starts_with("http") && !std::path::Path::new(&img).is_file() {
                    return Err(format!("No file at {img}."));
                }
                args = vec!["flash".into(), img.clone(), dpath, "-y".into()];
                if self.on("verify") {
                    args.push("--verify".into());
                }
                if self.on("eject") {
                    args.push("--eject".into());
                }
                rows.push(("Image", img));
                destructive = true;
                needs_finished = true;
            }
            Tool::Boot => match self.pick_val("action") {
                "Install" => {
                    args = vec!["boot".into(), "install".into(), dpath, "--label".into(), self.text("label"), "-y".into()];
                    destructive = true;
                    needs_finished = true;
                }
                "Update" => {
                    args = vec!["boot".into(), "update".into(), dpath];
                    needs_finished = true;
                }
                "Add ISO" => {
                    let files: Vec<String> = split_paths(&self.text("file"));
                    if files.is_empty() {
                        return Err("Enter the path of an ISO.".into());
                    }
                    for p in &files {
                        if !std::path::Path::new(p).is_file() {
                            return Err(format!("No file at {p}."));
                        }
                    }
                    args = vec!["boot".into(), "add".into(), dpath];
                    args.extend(files);
                    needs_finished = true;
                }
                "Remove" => {
                    let it = self.text("item");
                    if it.is_empty() {
                        return Err("Enter the item to remove (List shows them).".into());
                    }
                    args = vec!["boot".into(), "rm".into(), dpath, it];
                    needs_finished = true;
                }
                _ => {
                    args = vec!["boot".into(), "ls".into(), dpath];
                    special = Special::BootLs;
                }
            },
            Tool::Format => {
                let fs = match self.pick_val("fs") {
                    "FAT32" => Some("fat32"),
                    "exFAT" => Some("exfat"),
                    "NTFS" => Some("ntfs"),
                    "ext4" => Some("ext4"),
                    _ => None,
                };
                args = vec!["format".into(), dpath];
                if let Some(fs) = fs {
                    args.extend(["--fs".into(), fs.into()]);
                }
                args.extend(["--label".into(), self.text("label")]);
                args.extend(["--scheme".into(), self.pick_val("scheme").to_lowercase()]);
                args.extend(["--wipe".into(), self.pick_val("wipe").to_lowercase()]);
                match self.pick_val("cap") {
                    "Quick" => args.extend(["--test-capacity".into(), "quick".into()]),
                    "Full" => args.extend(["--test-capacity".into(), "full".into()]),
                    _ => {}
                }
                args.push("-y".into());
                destructive = true;
                needs_finished = true;
            }
            Tool::Test => {
                args = vec!["test".into(), dpath, "--preset".into(), self.pick_val("preset").to_lowercase(), "-y".into()];
                if self.on("erase") {
                    args.push("--erase".into());
                    destructive = true;
                }
                special = Special::Test;
            }
        }
        if let Some(c) = &cand {
            args.extend(c.flags());
        }
        if let Some(c) = &cand {
            let mut r = vec![("Drive", format!("{} · {} · {}", c.name, c.size_text(), c.drive.path))];
            r.append(&mut rows);
            rows = r;
        }
        Ok(Pending { args, cand, purpose: self.purpose, destructive, needs_finished, special, rows, verb: self.start_label() })
    }
}

fn expand(p: &str) -> String {
    let p = p.trim().trim_matches(|c| c == '"' || c == '\'');
    let p = p.strip_prefix("file://").unwrap_or(p);
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(h) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            return std::path::Path::new(&h).join(rest).to_string_lossy().into_owned();
        }
    }
    p.to_string()
}

/// Paths separated by spaces; a quoted path may contain spaces. A single existing file is never split.
fn split_paths(s: &str) -> Vec<String> {
    let one = expand(s);
    if std::path::Path::new(&one).is_file() {
        return vec![one];
    }
    let mut out = vec![];
    let (mut cur, mut q) = (String::new(), None::<char>);
    for c in s.chars() {
        match (c, q) {
            ('"' | '\'', None) => q = Some(c),
            (c, Some(x)) if c == x => q = None,
            (' ', None) => {
                if !cur.is_empty() {
                    out.push(expand(&std::mem::take(&mut cur)));
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(expand(&cur));
    }
    out
}

pub struct App {
    pub pages: Vec<Page>,
    pub cur: usize,
    pub side: bool,
    pub quit: bool,
    pub launch: Option<Box<Pending>>,
    pub changelog: Vec<Release>,
    pub about_scroll: u16,
    /// Showing the version history instead of a tool.
    pub about: bool,
    pub engine_ok: bool,
}

pub type Confirm<'a> = (&'a [(&'static str, String)], bool, &'static str);

pub struct Release {
    pub version: String,
    pub title: String,
    pub items: Vec<String>,
}

fn parse_changelog(src: &str) -> Vec<Release> {
    let mut out: Vec<Release> = vec![];
    for line in src.lines() {
        if let Some(h) = line.strip_prefix("## ") {
            let (v, t) = h.split_once('|').unwrap_or((h, ""));
            out.push(Release { version: v.trim().into(), title: t.trim().into(), items: vec![] });
        } else if let (Some(b), Some(r)) = (line.strip_prefix("- "), out.last_mut()) {
            r.items.push(b.trim().into());
        }
    }
    out
}

impl App {
    pub fn new() -> App {
        let pages = TOOLS.iter().map(|(t, _)| Page::new(*t)).collect();
        App { pages, cur: 0, side: false, quit: false, launch: None, changelog: parse_changelog(include_str!("../../nyforge-gui/src/changelog.txt")), about_scroll: 0, about: false, engine_ok: engine::engine_found() }
    }

    pub fn page(&self) -> &Page {
        &self.pages[self.cur]
    }

    fn page_mut(&mut self) -> &mut Page {
        &mut self.pages[self.cur]
    }

    pub fn busy(&self) -> bool {
        self.pages.iter().any(|p| matches!(p.mode, Mode::Running))
    }

    pub fn goto(&mut self, i: usize) {
        self.about = false;
        if i < self.pages.len() && i != self.cur {
            self.cur = i;
            if matches!(self.page().mode, Mode::Form) {
                self.page_mut().refresh();
            }
        }
    }

    /// Reads engine output for every running job. True if the screen changed.
    pub fn tick(&mut self) -> bool {
        let mut changed = false;
        for p in &mut self.pages {
            if !matches!(p.mode, Mode::Running) {
                continue;
            }
            changed = true;
            let kind = p.special_kind();
            let Some(run) = p.run.as_mut() else { continue };
            run.pump();
            if run.is_done() {
                let d = finish(run, kind);
                p.mode = Mode::Done(d);
                p.scroll = 0;
            }
        }
        changed
    }

    pub fn paste(&mut self, s: &str) {
        if self.page().editing {
            let s: String = s.chars().filter(|c| *c != '\n' && *c != '\r').collect();
            self.page_mut().text_edit(|t| t.push_str(&s));
        }
    }

    pub fn key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && k.code == KeyCode::Char('c') {
            if self.busy() {
                self.cancel_running();
            } else {
                self.quit = true;
            }
            return;
        }
        // text entry swallows everything
        if self.page().editing {
            let p = self.page_mut();
            match k.code {
                KeyCode::Enter | KeyCode::Esc | KeyCode::Down | KeyCode::Up => {
                    p.editing = false;
                    p.text_edit(|s| *s = s.trim().to_string());
                }
                KeyCode::Backspace => p.text_edit(|s| {
                    s.pop();
                }),
                KeyCode::Char('u') if ctrl => p.text_edit(|s| s.clear()),
                KeyCode::Char(c) if !ctrl => p.text_edit(|s| s.push(c)),
                _ => {}
            }
            return;
        }
        // keys that mean the same everywhere
        match k.code {
            KeyCode::Char('v') if !matches!(self.page().mode, Mode::Confirm(_)) => {
                self.about = !self.about;
                self.about_scroll = 0;
                self.side = false;
                return;
            }
            KeyCode::Char(c @ '1'..='4') if !matches!(self.page().mode, Mode::Confirm(_)) => {
                self.goto(c as usize - '1' as usize);
                return;
            }
            KeyCode::Tab => {
                self.side = !self.side;
                return;
            }
            _ => {}
        }
        if self.side {
            match k.code {
                KeyCode::Up | KeyCode::Char('k') if self.about => self.about = false,
                KeyCode::Up | KeyCode::Char('k') => self.goto(self.cur.saturating_sub(1)),
                KeyCode::Down | KeyCode::Char('j') if self.cur + 1 == self.pages.len() => self.about = true,
                KeyCode::Down | KeyCode::Char('j') => self.goto((self.cur + 1).min(self.pages.len() - 1)),
                KeyCode::Right | KeyCode::Enter | KeyCode::Esc | KeyCode::Char('l') => self.side = false,
                KeyCode::Char('q') if !self.busy() => self.quit = true,
                _ => {}
            }
            return;
        }
        if self.about {
            match k.code {
                KeyCode::Up | KeyCode::Char('k') => self.about_scroll = self.about_scroll.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => self.about_scroll = self.about_scroll.saturating_add(1),
                KeyCode::PageUp => self.about_scroll = self.about_scroll.saturating_sub(10),
                KeyCode::PageDown => self.about_scroll = self.about_scroll.saturating_add(10),
                KeyCode::Home => self.about_scroll = 0,
                KeyCode::Char('q') => self.quit = true,
                KeyCode::Esc | KeyCode::Left | KeyCode::Enter => self.about = false,
                _ => {}
            }
            return;
        }
        let mode = std::mem::replace(&mut self.page_mut().mode, Mode::Form);
        match mode {
            Mode::Form => self.form_key(k),
            Mode::Confirm(p) => {
                match k.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                        self.launch = Some(p);
                        self.page_mut().mode = Mode::Running; // shown as running while sudo is asked
                        return;
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc | KeyCode::Enter => {}
                    _ => {
                        self.page_mut().mode = Mode::Confirm(p);
                        return;
                    }
                }
                self.page_mut().mode = Mode::Form;
            }
            Mode::Running => {
                self.page_mut().mode = Mode::Running;
                match k.code {
                    KeyCode::Char('c') | KeyCode::Esc => self.cancel_running(),
                    _ => {}
                }
            }
            Mode::Done(d) => {
                let p = self.page_mut();
                match k.code {
                    KeyCode::Up | KeyCode::Char('k') => p.scroll = p.scroll.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => p.scroll = p.scroll.saturating_add(1),
                    KeyCode::PageUp => p.scroll = p.scroll.saturating_sub(10),
                    KeyCode::PageDown => p.scroll = p.scroll.saturating_add(10),
                    KeyCode::Char('q') => {
                        p.mode = Mode::Done(d);
                        self.quit = true;
                        return;
                    }
                    _ => {
                        p.run = None;
                        p.mode = Mode::Form;
                        p.refresh();
                        return;
                    }
                }
                self.page_mut().mode = Mode::Done(d);
            }
        }
    }

    /// A left click: a tool in the menu, or the version at the bottom.
    pub fn click(&mut self, x: u16, y: u16, height: u16) {
        if self.page().editing || x >= crate::ui::SIDEBAR_W {
            return;
        }
        if y + 2 == height {
            self.about = !self.about;
            self.about_scroll = 0;
        } else if y >= crate::ui::TOOLS_Y && ((y - crate::ui::TOOLS_Y) as usize) < self.pages.len() {
            self.goto((y - crate::ui::TOOLS_Y) as usize);
        }
        self.side = false;
    }

    fn cancel_running(&mut self) {
        for p in &mut self.pages {
            if matches!(p.mode, Mode::Running) {
                if let Some(r) = p.run.as_mut() {
                    r.job.cancel();
                }
            }
        }
    }

    fn form_key(&mut self, k: KeyEvent) {
        let p = self.page_mut();
        p.msg = None;
        let rows = p.rows();
        match k.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Esc => self.side = true,
            KeyCode::Left if p.cur().is_none_or(|i| !matches!(p.fields[i].kind, Kind::Pick(..) | Kind::Drive | Kind::Toggle(_))) => self.side = true,
            KeyCode::Up | KeyCode::Char('k') => p.focus = p.focus.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => p.focus = (p.focus + 1).min(rows - 1),
            KeyCode::Char('r') => {
                p.refresh();
                p.msg = Some("Drives refreshed.".into());
            }
            KeyCode::Left | KeyCode::Char('h') => step(p, -1),
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(' ') => step(p, 1),
            KeyCode::Enter => {
                if p.focus + 1 == rows {
                    self.start();
                } else if let Some(i) = p.cur() {
                    match &mut p.fields[i].kind {
                        Kind::Text(_) => p.editing = true,
                        Kind::Toggle(b) => *b = !*b,
                        _ => p.focus = (p.focus + 1).min(rows - 1),
                    }
                }
            }
            _ => {}
        }
    }

    fn start(&mut self) {
        let p = self.page_mut();
        match p.build() {
            Err(e) => p.msg = Some(e),
            Ok(pending) => {
                if let Some(c) = &pending.cand {
                    if let Err(e) = drives::revalidate(pending.purpose, c) {
                        p.msg = Some(e);
                        p.refresh();
                        return;
                    }
                }
                if pending.destructive {
                    p.mode = Mode::Confirm(Box::new(pending));
                } else {
                    p.mode = Mode::Running;
                    self.launch = Some(Box::new(pending));
                }
            }
        }
    }

    /// The main loop calls this once it has made sure the right to use drives is there (`authorised`).
    pub fn launch_now(&mut self, authorised: Result<(), String>) {
        let Some(p) = self.launch.take() else { return };
        let page = self.page_mut();
        if let Err(e) = authorised {
            page.mode = Mode::Done(Done { ok: Some(false), title: "Administrator access was not granted".into(), lines: vec![e, "Nothing was changed.".into()] });
            return;
        }
        if let Some(c) = &p.cand {
            if let Err(e) = drives::revalidate(p.purpose, c) {
                page.mode = Mode::Done(Done { ok: Some(false), title: "Nothing was changed".into(), lines: vec![e] });
                return;
            }
        }
        let sudo = engine::needs_sudo() && p.cand.is_some();
        let job = engine::spawn(&p.args, sudo);
        page.run = Some(Run::new(job, p.needs_finished));
        page.special = p.special == Special::Test;
        page.verb = p.verb;
        page.mode = Mode::Running;
    }

    pub fn wants_sudo(&self) -> bool {
        self.launch.as_ref().is_some_and(|p| p.cand.is_some()) && engine::needs_sudo()
    }

    pub fn confirm_rows(&self) -> Option<Confirm<'_>> {
        match &self.page().mode {
            Mode::Confirm(p) => Some((&p.rows, p.destructive, p.verb)),
            _ => None,
        }
    }
}

impl Page {
    fn special_kind(&self) -> Special {
        // The kind is decided when the job starts; the test report is the only special case that changes the result.
        if self.special {
            Special::Test
        } else if self.tool == Tool::Boot && self.pick_val("action") == "List" {
            Special::BootLs
        } else {
            Special::None
        }
    }
}

fn step(p: &mut Page, dir: i32) {
    let rows = p.rows();
    if p.focus + 1 == rows {
        return;
    }
    let Some(i) = p.cur() else { return };
    let n = p.drives.len();
    let sel = p.drive_sel;
    match &mut p.fields[i].kind {
        Kind::Pick(o, s) => *s = (*s as i32 + dir).rem_euclid(o.len() as i32) as usize,
        Kind::Toggle(b) => *b = !*b,
        Kind::Drive => {
            if n == 0 {
                return;
            }
            let mut j = sel.map_or(if dir > 0 { n - 1 } else { 0 }, |s| s);
            for _ in 0..n {
                j = (j as i32 + dir).rem_euclid(n as i32) as usize;
                if p.drives[j].blocked.is_none() {
                    p.drive_sel = Some(j);
                    break;
                }
            }
        }
        Kind::Text(_) => {}
    }
    p.verb = p.start_label();
    // the visible set can change (NyBoot actions)
    p.focus = p.focus.min(p.rows() - 1);
}

fn finish(run: &mut Run, special: Special) -> Done {
    let took = human_duration(run.started.elapsed().as_secs_f64());
    match run.outcome() {
        Outcome::Cancelled => Done { ok: None, title: "Cancelled".into(), lines: vec!["Stopped. The drive may hold partial data.".into()] },
        Outcome::Failed(m, h) => {
            let mut lines = vec![m];
            lines.extend(h);
            Done { ok: Some(false), title: "Failed".into(), lines }
        }
        Outcome::Running => Done { ok: Some(false), title: "Failed".into(), lines: vec![] },
        Outcome::Success => {
            let mut lines = vec![];
            if let Some((secs, bps)) = run.finished {
                lines.push(format!("Written in {} at {}.", human_duration(secs), human_speed(bps)));
            }
            if let Some((secs, _)) = run.verified {
                lines.push(format!("Read back and verified in {}.", human_duration(secs)));
            }
            if let Some(n) = &run.verified_note {
                lines.push(n.clone());
            }
            match special {
                Special::BootLs => {
                    if let Some(v) = run.find_json("items") {
                        let free = v["free"].as_u64().unwrap_or(0);
                        let total = v["total"].as_u64().unwrap_or(0);
                        lines.push(format!("{} free of {}", human_size(free), human_size(total)));
                        lines.push(String::new());
                        let items = v["items"].as_array().cloned().unwrap_or_default();
                        if items.is_empty() {
                            lines.push("Nothing on the drive yet. Use Add ISO.".into());
                        }
                        for it in items {
                            let path = it["path"].as_str().unwrap_or("");
                            let size = if it["dir"].as_bool().unwrap_or(false) { "folder".to_string() } else { human_size(it["size"].as_u64().unwrap_or(0)) };
                            lines.push(format!("{size:>10}  {path}"));
                        }
                    }
                }
                Special::Test => {
                    for v in run.json.iter().rev() {
                        if let Ok(nyforge_core::nytest::TestEvent::NytestDone { report }) = serde_json::from_value::<nyforge_core::nytest::TestEvent>(v.clone()) {
                            lines.extend(nyforge_core::nytest::report_text(&report).lines().map(|l| l.to_string()));
                            break;
                        }
                    }
                }
                Special::None => {
                    if lines.is_empty() {
                        lines.push(format!("Finished in {took}."));
                    }
                }
            }
            if matches!(special, Special::None) && run.finished.is_some() {
                lines.push("You can unplug the drive.".into());
            }
            Done { ok: Some(true), title: "Done".into(), lines }
        }
    }
}

/// What the running screen shows.
pub struct Status {
    pub heading: String,
    pub fraction: Option<f64>,
    pub stats: String,
    pub detail: String,
}

pub fn status(p: &Page) -> Status {
    let Some(run) = p.run.as_ref() else {
        return Status { heading: "Waiting for administrator access".into(), fraction: None, stats: String::new(), detail: String::new() };
    };
    let verifying = run.is_verifying();
    let heading = match &run.phase {
        Some((Phase::Verifying, _)) => "Reading back".to_string(),
        Some((Phase::Writing, _)) => "Writing".to_string(),
        Some((Phase::Finalizing, _)) => "Finishing".to_string(),
        Some((Phase::Preparing, _)) => "Preparing".to_string(),
        None => "Starting".to_string(),
    };
    let mut stats = vec![];
    if let Some(f) = run.fraction() {
        stats.push(format!("{:.0}%", f * 100.0));
    }
    if let Some(s) = run.smooth.speed() {
        stats.push(human_speed(s));
    }
    if let Some(e) = run.smooth.eta() {
        stats.push(format!("{} left", human_duration(e)));
    }
    stats.push(format!("{} elapsed", human_duration(run.started.elapsed().as_secs_f64())));
    let mut detail = run.phase.as_ref().map(|(_, d)| d.clone()).unwrap_or_default();
    // NyTest reports its own steps
    if p.special {
        for v in run.json.iter().rev() {
            match serde_json::from_value::<nyforge_core::nytest::TestEvent>(v.clone()) {
                Ok(nyforge_core::nytest::TestEvent::NytestLive { label, fraction, mbps, .. }) => {
                    return Status { heading: label, fraction: Some(fraction.clamp(0.0, 1.0)), stats: format!("{:.0}%  ·  {:.1} MB/s", fraction * 100.0, mbps), detail: String::new() };
                }
                Ok(nyforge_core::nytest::TestEvent::NytestStep { label, index, of, .. }) => {
                    return Status { heading: label, fraction: None, stats: format!("step {} of {}", index + 1, of), detail: String::new() };
                }
                _ => {}
            }
        }
    }
    if verifying && detail.is_empty() {
        detail = "Comparing the drive with the image".into();
    }
    Status { heading, fraction: run.fraction(), stats: stats.join("  ·  "), detail }
}
