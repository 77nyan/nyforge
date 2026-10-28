//! NyBoot's menu settings, kept in `/nyboot/nyboot.cfg` on the data partition (a GRUB script the menu reads at start),
//! and per-image boot options, kept in `<image>.nyboot.cfg` next to the image.
//!
//! NyForge only ever rewrites the block between its two marker lines. Anything else in `nyboot.cfg`, such as lines the
//! user wrote by hand, stays exactly as it is.

use super::drive::OpenDrive;
use crate::error::{NyError, Result};

pub const CONFIG_PATH: &str = "nyboot/nyboot.cfg";
const BEGIN: &str = "# ---- NyForge settings (managed by NyForge, edit with `nyforge boot config`) ----";
const END: &str = "# ---- end of NyForge settings ----";

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct MenuConfig {
    /// Seconds before the default entry starts by itself. `None`: wait for a choice.
    pub timeout_secs: Option<u32>,
    /// Name of the entry (the image's file name without `.iso`) that is preselected.
    pub default_entry: Option<String>,
    /// Plain text menu instead of the graphical one (for old or odd graphics hardware).
    pub text_mode: bool,
    /// How many folder levels below the top of the drive are searched for images. `None`: the default (3).
    pub depth: Option<u32>,
    /// Name of a custom menu theme: the folder `/nyboot/themes/<name>/` on the data partition. `None`: the built-in one.
    pub theme: Option<String>,
}

pub const MAX_TIMEOUT: u32 = 3600;
pub const MAX_DEPTH: u32 = 8;

/// Text that goes inside a GRUB double-quoted string: refuse anything GRUB would interpret.
pub fn check_quoted(what: &str, s: &str) -> Result<()> {
    if s.chars().any(|c| c.is_control() || matches!(c, '"' | '$' | '\\' | '`')) {
        return Err(NyError::invalid(format!("{what} cannot contain quotes, $, backslashes or control characters.")));
    }
    Ok(())
}

impl MenuConfig {
    pub fn validate(&self) -> Result<()> {
        if let Some(t) = self.timeout_secs {
            if t > MAX_TIMEOUT {
                return Err(NyError::invalid(format!("The timeout can be at most {MAX_TIMEOUT} seconds.")));
            }
        }
        if let Some(d) = self.depth {
            if d > MAX_DEPTH {
                return Err(NyError::invalid(format!("The folder depth can be at most {MAX_DEPTH}.")));
            }
        }
        if let Some(n) = &self.default_entry {
            if n.trim().is_empty() {
                return Err(NyError::invalid("The default entry is empty."));
            }
            check_quoted("The default entry", n)?;
        }
        if let Some(t) = &self.theme {
            check_theme_name(t)?;
        }
        Ok(())
    }

    /// The managed block, including its marker lines (empty settings give an empty string).
    pub fn render(&self) -> String {
        if *self == MenuConfig::default() {
            return String::new();
        }
        let mut s = format!("{BEGIN}\n");
        if let Some(t) = self.timeout_secs {
            s.push_str(&format!("set nb_timeout={t}\n"));
        }
        if let Some(n) = &self.default_entry {
            s.push_str(&format!("set nb_default=\"{n}\"\n"));
        }
        if self.text_mode {
            s.push_str("set nb_text=1\n");
        }
        if let Some(d) = self.depth {
            s.push_str(&format!("set nb_maxdepth=\"{}\"\n", "x".repeat(d as usize + 1)));
        }
        if let Some(t) = &self.theme {
            s.push_str(&format!("set nb_theme=\"{t}\"\n"));
        }
        s.push_str(END);
        s.push('\n');
        s
    }

    /// Read the managed block out of a `nyboot.cfg` (settings written some other way are not picked up).
    pub fn parse(text: &str) -> MenuConfig {
        let mut c = MenuConfig::default();
        let mut inside = false;
        for line in text.lines() {
            let l = line.trim();
            if l == BEGIN {
                inside = true;
                continue;
            }
            if l == END {
                break;
            }
            if !inside {
                continue;
            }
            let Some(rest) = l.strip_prefix("set ") else { continue };
            let Some((k, v)) = rest.split_once('=') else { continue };
            let v = v.trim().trim_matches('"');
            match k.trim() {
                "nb_timeout" => c.timeout_secs = v.parse().ok(),
                "nb_default" => c.default_entry = Some(v.to_string()),
                "nb_text" => c.text_mode = !v.is_empty(),
                "nb_theme" => c.theme = Some(v.to_string()).filter(|t| !t.is_empty()),
                "nb_maxdepth" => c.depth = Some((v.len() as u32).saturating_sub(1)),
                _ => {}
            }
        }
        c
    }

    /// `existing` with the managed block replaced (or added, or removed when this config is empty).
    pub fn apply_to(&self, existing: &str) -> String {
        let mut out = String::new();
        let mut skipping = false;
        for line in existing.lines() {
            let l = line.trim();
            if l == BEGIN {
                skipping = true;
                continue;
            }
            if skipping {
                if l == END {
                    skipping = false;
                }
                continue;
            }
            out.push_str(line);
            out.push('\n');
        }
        let block = self.render();
        if !block.is_empty() {
            if !out.is_empty() && !out.ends_with("\n\n") {
                out.push('\n');
            }
            out.push_str(&block);
        }
        out
    }
}

/// A theme is a folder name: letters, digits, space, dot, dash, underscore. (`off` is reserved: no theme at all.)
pub fn check_theme_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && name.chars().all(|c| c.is_alphanumeric() || matches!(c, ' ' | '.' | '-' | '_'));
    if !ok {
        return Err(NyError::invalid("A theme name can have letters, digits, spaces, dots, dashes and underscores (at most 64), and cannot start with a dot."));
    }
    Ok(())
}

pub const THEMES_DIR: &str = "nyboot/themes";
const MAX_THEME_FILE: u64 = 8 << 20;
const MAX_THEME_TOTAL: u64 = 48 << 20;

fn remove_tree(od: &mut OpenDrive, path: &str) -> Result<()> {
    for e in od.fs.list(path)? {
        let child = format!("{path}/{}", e.name);
        if e.is_dir {
            remove_tree(od, &child)?;
        } else {
            od.fs.remove(&child)?;
        }
    }
    od.fs.remove(path)
}

/// Copy a GRUB theme folder (it must hold a `theme.txt`) to `/nyboot/themes/<name>/` on the data partition.
/// Any GRUB 2 theme works, the same ones Ventoy takes. Replaces a theme of the same name. Returns the number of files.
pub fn install_theme(od: &mut OpenDrive, src: &std::path::Path, name: &str) -> Result<usize> {
    check_theme_name(name)?;
    if !src.join("theme.txt").is_file() {
        return Err(NyError::invalid(format!("{} has no theme.txt.", src.display()))
            .with_hint("Point at the theme's own folder, the one that holds theme.txt."));
    }
    let mut files: Vec<(std::path::PathBuf, String, u64)> = Vec::new();
    let mut total = 0u64;
    let mut stack = vec![(src.to_path_buf(), String::new())];
    while let Some((dir, rel)) = stack.pop() {
        for e in std::fs::read_dir(&dir)? {
            let e = e?;
            let fname = e.file_name().to_string_lossy().into_owned();
            let meta = std::fs::metadata(e.path())?;
            let r = if rel.is_empty() { fname.clone() } else { format!("{rel}/{fname}") };
            if fname.starts_with('.') {
                continue;
            }
            if meta.is_dir() {
                if r.matches('/').count() < 3 {
                    stack.push((e.path(), r));
                }
            } else if meta.is_file() {
                if meta.len() > MAX_THEME_FILE {
                    return Err(NyError::invalid(format!("{r} is larger than {} MiB, too big for a theme file.", MAX_THEME_FILE >> 20)));
                }
                total += meta.len();
                files.push((e.path(), r, meta.len()));
            }
        }
    }
    if total > MAX_THEME_TOTAL {
        return Err(NyError::invalid(format!("This theme is {} MiB; the limit is {} MiB.", total >> 20, MAX_THEME_TOTAL >> 20)));
    }
    let base = format!("{THEMES_DIR}/{name}");
    if stat_opt(od, &base)?.is_some() {
        remove_tree(od, &base)?;
    }
    od.fs.mkdir_p(&base)?;
    for (path, rel, len) in &files {
        if let Some((dir, _)) = rel.rsplit_once('/') {
            od.fs.mkdir_p(&format!("{base}/{dir}"))?;
        }
        let mut f = std::fs::File::open(path)?;
        od.fs.add_file(&format!("{base}/{rel}"), &mut f, *len, &mut |_| true)?;
    }
    od.fs.sync()?;
    Ok(files.len())
}

/// `stat` that treats "the folder is not there" as "the file is not there".
fn stat_opt(od: &OpenDrive, path: &str) -> Result<Option<super::exfat::Entry>> {
    match od.fs.stat(path) {
        Err(e) if e.kind == crate::error::ErrorKind::InvalidInput => Ok(None),
        other => other,
    }
}

/// A small text file from the data partition, if it exists.
pub fn read_text(od: &OpenDrive, path: &str) -> Result<Option<String>> {
    let Some(e) = stat_opt(od, path)? else { return Ok(None) };
    if e.is_dir || e.size > 1 << 20 {
        return Ok(None);
    }
    let mut buf = vec![0u8; e.size as usize];
    let n = od.fs.read_file(&e, 0, &mut buf)?;
    buf.truncate(n);
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}

/// Create or replace a small text file on the data partition (an empty text removes it).
pub fn write_text(od: &mut OpenDrive, path: &str, text: &str) -> Result<()> {
    if stat_opt(od, path)?.is_some() {
        od.fs.remove(path)?;
    }
    if !text.is_empty() {
        if let Some((dir, _)) = path.rsplit_once('/') {
            od.fs.mkdir_p(dir)?;
        }
        let bytes = text.as_bytes();
        od.fs.add_file(path, &mut &bytes[..], bytes.len() as u64, &mut |_| true)?;
    }
    od.fs.sync()
}

pub fn load(od: &OpenDrive) -> Result<MenuConfig> {
    Ok(read_text(od, CONFIG_PATH)?.map(|t| MenuConfig::parse(&t)).unwrap_or_default())
}

pub fn save(od: &mut OpenDrive, cfg: &MenuConfig) -> Result<()> {
    cfg.validate()?;
    let existing = read_text(od, CONFIG_PATH)?.unwrap_or_default();
    write_text(od, CONFIG_PATH, &cfg.apply_to(&existing))
}

// ------------------------------------------------------------------ per-image options

fn params_path(iso: &str) -> String {
    format!("{}.nyboot.cfg", iso.trim_start_matches('/'))
}

/// The boot options set for one image with `set_params` (not hand-written ones).
pub fn get_params(od: &OpenDrive, iso: &str) -> Result<Option<String>> {
    Ok(read_text(od, &params_path(iso))?.and_then(|t| {
        t.lines().find_map(|l| l.trim().strip_prefix("set nb_params=\"").and_then(|r| r.strip_suffix('"')).map(str::to_string))
    }))
}

/// Extra kernel options for one image, such as `nomodeset`. `None` or empty removes them.
pub fn set_params(od: &mut OpenDrive, iso: &str, params: Option<&str>) -> Result<()> {
    if stat_opt(od, iso.trim_start_matches('/'))?.is_none() {
        return Err(NyError::invalid(format!("{iso} is not on the drive.")).with_hint("List the files with: nyforge boot ls <drive>"));
    }
    let text = match params.map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => {
            check_quoted("The boot options", p)?;
            format!("# Written by NyForge.\nset nb_params=\"{p}\"\n")
        }
        None => String::new(),
    };
    write_text(od, &params_path(iso), &text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_parse_roundtrip() {
        let c = MenuConfig { timeout_secs: Some(10), default_entry: Some("Ubuntu 24.04".into()), text_mode: true, depth: Some(2), theme: Some("My Dark".into()) };
        let t = c.render();
        assert!(t.contains("set nb_theme=\"My Dark\"\n") && t.contains("set nb_timeout=10\n") && t.contains("set nb_maxdepth=\"xxx\"\n"));
        assert_eq!(MenuConfig::parse(&t), c);
        assert_eq!(MenuConfig::default().render(), "");
    }

    #[test]
    fn hand_written_lines_survive() {
        let mine = "# my own\nset gfxmode=1024x768\n";
        let c = MenuConfig { timeout_secs: Some(5), ..Default::default() };
        let once = c.apply_to(mine);
        assert!(once.starts_with(mine));
        let c2 = MenuConfig { timeout_secs: Some(30), default_entry: Some("Arch".into()), ..Default::default() };
        let twice = c2.apply_to(&once);
        assert!(twice.starts_with(mine) && twice.contains("nb_timeout=30") && !twice.contains("nb_timeout=5"));
        assert_eq!(MenuConfig::parse(&twice), c2);
        // Resetting removes only our block.
        assert_eq!(MenuConfig::default().apply_to(&twice).trim_end(), mine.trim_end());
    }

    #[test]
    fn dangerous_text_is_refused() {
        for bad in ["a\"b", "a$b", "a\\b", "a`b", "a\nb"] {
            assert!(MenuConfig { default_entry: Some(bad.into()), ..Default::default() }.validate().is_err(), "{bad:?}");
        }
        assert!(MenuConfig { timeout_secs: Some(MAX_TIMEOUT + 1), ..Default::default() }.validate().is_err());
        assert!(MenuConfig { depth: Some(MAX_DEPTH + 1), ..Default::default() }.validate().is_err());
        for bad in ["", "../x", "a/b", ".hid", "q\"x", "a$b"] {
            assert!(MenuConfig { theme: Some(bad.into()), ..Default::default() }.validate().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn settings_and_image_options_are_stored_on_the_drive() {
        use crate::nyboot::{drive, install::{install, InstallOptions}};
        use crate::writer::Target;
        use std::sync::{atomic::AtomicBool, Arc};
        let d = std::env::temp_dir().join(format!("nyforge-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let img = d.join("nb.img");
        let opts = InstallOptions { file_size: Some(768 << 20), esp_mib: 32, ..Default::default() };
        install(&Target::file(&img), &opts, Arc::new(AtomicBool::new(false)), &mut |_| {}).unwrap();
        let iso = d.join("one.iso");
        std::fs::write(&iso, vec![7u8; 100_000]).unwrap();
        let mut od = drive::open(&Target::file(&img), true).unwrap();
        drive::add_image(&mut od, crate::image::open_file(&iso).unwrap(), "isos", Arc::new(AtomicBool::new(false)), &mut |_| {}).unwrap();
        // The user's own lines are in the file first.
        write_text(&mut od, CONFIG_PATH, "set gfxmode=800x600\n").unwrap();
        let c = MenuConfig { timeout_secs: Some(3), default_entry: Some("one".into()), text_mode: true, depth: Some(1), theme: None };
        save(&mut od, &c).unwrap();
        drop(od);
        let mut od = drive::open(&Target::file(&img), true).unwrap();
        assert_eq!(load(&od).unwrap(), c);
        assert!(read_text(&od, CONFIG_PATH).unwrap().unwrap().starts_with("set gfxmode=800x600\n"));
        // A theme folder is copied, with its sub folders; one without theme.txt is refused.
        let th = d.join("mytheme");
        std::fs::create_dir_all(th.join("icons")).unwrap();
        std::fs::write(th.join("theme.txt"), "desktop-color: \"#101010\"\n").unwrap();
        std::fs::write(th.join("icons/a.png"), [1u8; 40]).unwrap();
        assert_eq!(install_theme(&mut od, &th, "mine").unwrap(), 2);
        assert!(read_text(&od, "nyboot/themes/mine/theme.txt").unwrap().unwrap().contains("#101010"));
        assert_eq!(install_theme(&mut od, &th, "mine").unwrap(), 2, "installing again replaces it");
        assert!(install_theme(&mut od, &d, "nope").is_err());
        assert!(install_theme(&mut od, &th, "../x").is_err());
        set_params(&mut od, "isos/one.iso", Some("nomodeset quiet")).unwrap();
        assert_eq!(get_params(&od, "isos/one.iso").unwrap().as_deref(), Some("nomodeset quiet"));
        assert!(set_params(&mut od, "isos/missing.iso", Some("x")).is_err());
        assert!(set_params(&mut od, "isos/one.iso", Some("a\"; reboot; \"")).is_err());
        set_params(&mut od, "isos/one.iso", None).unwrap();
        assert_eq!(get_params(&od, "isos/one.iso").unwrap(), None);
        save(&mut od, &MenuConfig::default()).unwrap();
        assert_eq!(load(&od).unwrap(), MenuConfig::default());
        assert_eq!(read_text(&od, CONFIG_PATH).unwrap().unwrap().trim(), "set gfxmode=800x600");
    }
}
