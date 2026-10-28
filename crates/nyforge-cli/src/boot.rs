//! `nyforge boot …` — NyBoot multiboot drives.

use crate::{confirm_erase, install_ctrlc, is_root, short_name, Res, EXIT_FAIL};
use crate::ui::Ui;
use clap::{Args, Subcommand};
use nyforge_core::device::{self, Drive, ResolvedTarget, TargetPolicy};
use nyforge_core::error::{ErrorKind, NyError};
use nyforge_core::format::{human_size, parse_size};
use nyforge_core::image;
use nyforge_core::nyboot::{self, drive, exfat::Entry, install::InstallOptions};
use nyforge_core::progress::Event;
use nyforge_core::writer::Target;

#[derive(Subcommand)]
pub enum BootCmd {
    /// Turn a drive (or an image file) into a NyBoot drive: copy ISOs onto it, boot any of them
    Install(InstallArgs),
    /// Copy ISO files (or .iso.xz/.gz/.zst, or URLs) onto a NyBoot drive
    Add(AddArgs),
    /// List what is on a NyBoot drive
    Ls(LsArgs),
    /// Remove a file from a NyBoot drive
    Rm(RmArgs),
    /// Look inside an ISO and say whether NyBoot can start it
    Check(CheckArgs),
    /// Put the newest boot code and menu on a NyBoot drive without touching the ISOs on it
    Update(UpdateArgs),
    /// Show or change the menu: timeout, preselected entry, text mode, folder depth
    Config(ConfigArgs),
    /// Set extra kernel options for one ISO on the drive (for example nomodeset)
    Params(ParamsArgs),
    /// Copy a GRUB 2 theme folder onto the drive (works with Ventoy-style themes) and optionally use it
    Theme(ThemeArgs),
}

#[derive(Args)]
pub struct ThemeArgs {
    /// NyBoot drive or image file
    pub target: String,
    /// The theme's folder on this computer (the one that holds theme.txt)
    pub folder: std::path::PathBuf,
    /// Name to store it under (default: the folder's name)
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,
    /// Do not switch the menu to this theme (it is only copied)
    #[arg(long)]
    pub keep_current: bool,
    #[command(flatten)]
    pub common: Common,
}

#[derive(Args)]
pub struct UpdateArgs {
    /// NyBoot drive or image file
    pub target: String,
    #[command(flatten)]
    pub common: Common,
}

#[derive(Args)]
pub struct ConfigArgs {
    /// NyBoot drive or image file
    pub target: String,
    /// Start the preselected entry after this many seconds (0 to 3600)
    #[arg(long, value_name = "SECONDS", conflicts_with = "no_timeout")]
    pub timeout: Option<u32>,
    /// Wait for a choice (no automatic start)
    #[arg(long)]
    pub no_timeout: bool,
    /// Preselect this entry: the image's name without .iso, e.g. "ubuntu-24.04"
    #[arg(long, value_name = "NAME", conflicts_with = "clear_default")]
    pub default: Option<String>,
    /// Preselect the first entry again
    #[arg(long)]
    pub clear_default: bool,
    /// Plain text menu instead of the graphical one: on or off
    #[arg(long, value_name = "on|off")]
    pub text_mode: Option<String>,
    /// How many folder levels are searched for images (0 to 8, default 3)
    #[arg(long, value_name = "LEVELS")]
    pub depth: Option<u32>,
    /// Use the theme stored under this name (see `nyforge boot theme`); "off" shows the menu without a theme
    #[arg(long, value_name = "NAME", conflicts_with = "clear_theme")]
    pub theme: Option<String>,
    /// Go back to the built-in NyBoot theme
    #[arg(long)]
    pub clear_theme: bool,
    /// Forget every setting NyForge made (lines you wrote yourself stay)
    #[arg(long)]
    pub reset: bool,
    #[command(flatten)]
    pub common: Common,
}

#[derive(Args)]
pub struct ParamsArgs {
    /// NyBoot drive or image file
    pub target: String,
    /// Path of the ISO on the drive, as `nyforge boot ls` shows it
    pub iso: String,
    /// Kernel options to add when this ISO starts, e.g. "nomodeset quiet"
    #[arg(conflicts_with = "clear")]
    pub options: Option<String>,
    /// Remove the options again
    #[arg(long)]
    pub clear: bool,
    #[command(flatten)]
    pub common: Common,
}

#[derive(Args)]
pub struct CheckArgs {
    /// ISO file
    pub iso: std::path::PathBuf,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct Common {
    #[arg(long)]
    pub allow_internal: bool,
    #[arg(long)]
    pub allow_virtual: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct InstallArgs {
    /// Drive (e.g. /dev/sdb) or an image file to create
    pub target: String,
    /// Size of the image file when the target is a file, e.g. 4G
    #[arg(long, value_name = "SIZE")]
    pub size: Option<String>,
    /// Name of the data partition
    #[arg(long, default_value = "NYBOOT")]
    pub label: String,
    /// Size of the UEFI boot partition in MiB
    #[arg(long, default_value_t = 64, value_name = "MIB")]
    pub esp_size: u64,
    /// Leave this much space unpartitioned at the end of the drive, in MiB
    #[arg(long, default_value_t = 0, value_name = "MIB")]
    pub reserve: u64,
    /// Do not ask for confirmation
    #[arg(short = 'y', long)]
    pub yes: bool,
    /// (testing) put this GRUB script on the boot partition as grub/test.cfg
    #[arg(long, hide = true, value_name = "FILE")]
    pub test_cfg: Option<std::path::PathBuf>,
    #[command(flatten)]
    pub common: Common,
}

#[derive(Args)]
pub struct AddArgs {
    /// NyBoot drive or image file
    pub target: String,
    /// ISO files or URLs
    #[arg(required = true, num_args = 1..)]
    pub images: Vec<String>,
    /// Folder on the drive to put them in
    #[arg(long, default_value = "", value_name = "DIR")]
    pub dir: String,
    #[command(flatten)]
    pub common: Common,
}

#[derive(Args)]
pub struct LsArgs {
    pub target: String,
    #[command(flatten)]
    pub common: Common,
}

#[derive(Args)]
pub struct RmArgs {
    pub target: String,
    /// Path on the drive, e.g. iso/ubuntu.iso
    pub path: String,
    #[command(flatten)]
    pub common: Common,
}

fn resolve(spec: &str, c: &Common, must_exist: bool) -> Result<Target, NyError> {
    let policy = TargetPolicy { allow_internal: c.allow_internal, allow_virtual: c.allow_virtual };
    let r = device::resolve_target(spec, policy)?;
    if must_exist {
        if let ResolvedTarget::File { path } = &r {
            if !std::path::Path::new(path).is_file() {
                return Err(NyError::new(ErrorKind::InvalidInput, format!("{path} does not exist.")));
            }
        }
    }
    Ok(Target::from_resolved(r))
}

fn need_root(t: &Target, what: &str) -> Result<(), NyError> {
    if t.drive.is_some() && !is_root() {
        return Err(NyError::new(ErrorKind::Permission, format!("{what} needs administrator rights."))
            .with_hint(nyforge_core::sys::elevation_hint()));
    }
    Ok(())
}

pub fn run(cmd: BootCmd) -> Res {
    match cmd {
        BootCmd::Install(a) => install(a),
        BootCmd::Add(a) => add(a),
        BootCmd::Ls(a) => ls(a),
        BootCmd::Rm(a) => rm(a),
        BootCmd::Check(a) => check(a),
        BootCmd::Update(a) => update(a),
        BootCmd::Config(a) => config(a),
        BootCmd::Params(a) => params(a),
        BootCmd::Theme(a) => theme(a),
    }
}

fn verdict_line(ui: &Ui, r: &nyboot::probe::IsoReport) -> String {
    use nyboot::probe::Verdict::*;
    match r.verdict {
        Expected => ui.green("Should boot from a NyBoot drive."),
        BestEffort => "Might boot from a NyBoot drive (best effort, not guaranteed).".to_string(),
        WindowsInstaller => ui.bold("Windows installer: use `nyforge windows`, not a multiboot drive."),
        Unknown => ui.red("NyBoot does not recognise how to start this image."),
    }
}

fn check(a: CheckArgs) -> Res {
    let r = nyboot::probe::analyze_file(&a.iso)?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&r).unwrap());
        return Ok(0);
    }
    let ui = Ui::new();
    println!("Image:     {}", a.iso.display());
    println!("Label:     {}", r.label);
    if let Some(d) = &r.distro {
        println!("Looks like: {d}");
    }
    println!("Format:    {} ({})", r.container, human_size(r.size));
    println!("Start via: {}", r.profile);
    println!("{}", verdict_line(&ui, &r));
    for n in &r.notes {
        println!("  - {n}");
    }
    Ok(0)
}

fn emitter(json: bool, ui: &mut Ui) -> impl FnMut(Event) + '_ {
    move |ev: Event| {
        if json {
            crate::print_event(&ev);
        } else {
            ui.render(&ev);
        }
    }
}

fn install(a: InstallArgs) -> Res {
    let t = resolve(&a.target, &a.common, false)?;
    need_root(&t, "Installing NyBoot on a drive")?;
    let file_size = match &a.size {
        Some(s) => Some(parse_size(s).ok_or_else(|| NyError::invalid(format!("\"{s}\" is not a size like 4G or 512M.")))?),
        None => None,
    };
    let drives: Vec<&Drive> = t.drive.iter().collect();
    if !drives.is_empty() && !a.yes {
        if a.common.json {
            return Err(NyError::new(ErrorKind::InvalidInput, "--json needs --yes: there is nobody to ask."));
        }
        let ui = Ui::new();
        confirm_erase(&drives, "NyBoot", &ui)?;
    }
    let mut opts = InstallOptions { label: a.label.clone(), esp_mib: a.esp_size, reserve_mib: a.reserve, file_size, ..Default::default() };
    if let Some(p) = &a.test_cfg {
        let bytes = std::fs::read(p).map_err(|e| NyError::from_io(&e, &format!("reading {}", p.display())))?;
        opts.extra_esp_files.push(("grub/test.cfg".into(), bytes));
    }
    let cancel = install_ctrlc();
    let mut ui = Ui::new();
    let res = {
        let mut emit = emitter(a.common.json, &mut ui);
        nyboot::install::install(&t, &opts, cancel, &mut emit)
    };
    ui.finish_line();
    let rep = res?;
    if !a.common.json {
        eprintln!(
            "{} NyBoot is ready on {}: {} for your ISOs ({}). Copy .iso files onto it with `nyforge boot add`, or just drag them in.",
            ui.green("✔"),
            t.display,
            human_size(rep.data_bytes),
            a.label
        );
    }
    Ok(0)
}

fn add(a: AddArgs) -> Res {
    let t = resolve(&a.target, &a.common, true)?;
    need_root(&t, "Writing to a drive")?;
    let mut od = drive::open(&t, true)?;
    let cancel = install_ctrlc();
    let mut ui = Ui::new();
    let mut failed = 0u8;
    for spec in &a.images {
        if !image::is_url(spec) && spec.to_ascii_lowercase().ends_with(".iso") {
            if let Ok(r) = nyboot::probe::analyze_file(std::path::Path::new(spec)) {
                if matches!(r.verdict, nyboot::probe::Verdict::WindowsInstaller | nyboot::probe::Verdict::Unknown) {
                    ui.finish_line();
                    eprintln!("{} {}: {}", ui.bold("Heads up:"), spec, r.notes.first().map(String::as_str).unwrap_or("this image may not start from a multiboot drive."));
                }
            }
        }
        let src = match image::open(spec) {
            Ok(s) => s,
            Err(e) => {
                ui.finish_line();
                Ui::print_error(&e);
                failed = EXIT_FAIL;
                continue;
            }
        };
        let r = {
            let mut emit = emitter(a.common.json, &mut ui);
            drive::add_image(&mut od, src, &a.dir, cancel.clone(), &mut emit)
        };
        ui.finish_line();
        match r {
            Ok(e) => {
                if !a.common.json {
                    eprintln!("{} Added {} ({})", ui.green("✔"), e.name, human_size(e.size));
                }
            }
            Err(e) if e.kind == ErrorKind::Cancelled => return Err(e),
            Err(e) => {
                Ui::print_error(&e);
                failed = EXIT_FAIL;
            }
        }
    }
    Ok(failed)
}

fn walk(od: &drive::OpenDrive, dir: &str, out: &mut Vec<(String, Entry)>) -> Result<(), NyError> {
    for e in od.fs.list(dir)? {
        let path = if dir.is_empty() || dir == "/" { e.name.clone() } else { format!("{}/{}", dir.trim_matches('/'), e.name) };
        if e.is_dir {
            out.push((format!("{path}/"), e.clone()));
            walk(od, &path, out)?;
        } else {
            out.push((path, e));
        }
    }
    Ok(())
}

fn ls(a: LsArgs) -> Res {
    let t = resolve(&a.target, &a.common, true)?;
    need_root(&t, "Reading a drive")?;
    let od = drive::open(&t, true)?;
    let mut items = vec![];
    walk(&od, "/", &mut items)?;
    if a.common.json {
        let v: Vec<_> = items
            .iter()
            .map(|(p, e)| serde_json::json!({"path": p, "size": e.size, "dir": e.is_dir}))
            .collect();
        println!("{}", serde_json::json!({"free": od.fs.free_bytes(), "total": od.data_bytes, "items": v}));
        return Ok(0);
    }
    if items.is_empty() {
        println!("No files yet. Add ISOs with: nyforge boot add {} some.iso", short_name(&a.target));
    }
    for (p, e) in &items {
        if e.is_dir {
            println!("{:>10}  {}", "", p);
        } else {
            println!("{:>10}  {}", human_size(e.size), p);
        }
    }
    println!("{} free of {}", human_size(od.fs.free_bytes()), human_size(od.data_bytes));
    Ok(0)
}

fn rm(a: RmArgs) -> Res {
    let t = resolve(&a.target, &a.common, true)?;
    need_root(&t, "Writing to a drive")?;
    let mut od = drive::open(&t, true)?;
    od.fs.remove(&a.path)?;
    if !a.common.json {
        eprintln!("Removed {}", a.path);
    }
    Ok(0)
}

fn update(a: UpdateArgs) -> Res {
    let t = resolve(&a.target, &a.common, true)?;
    need_root(&t, "Updating a drive")?;
    let cancel = install_ctrlc();
    let mut ui = Ui::new();
    let res = {
        let mut emit = emitter(a.common.json, &mut ui);
        nyboot::update::update(&t, true, cancel, &mut emit)
    };
    ui.finish_line();
    let rep = res?;
    if !a.common.json {
        eprintln!(
            "{} NyBoot on {} is updated ({} -> {}). Your ISOs and settings were not touched.",
            ui.green("✔"),
            t.display,
            rep.from.as_deref().unwrap_or("unknown version"),
            rep.to
        );
    }
    Ok(0)
}

fn show_config(c: &nyboot::config::MenuConfig, json: bool) {
    if json {
        println!("{}", serde_json::to_string(c).unwrap());
        return;
    }
    println!("Timeout:        {}", c.timeout_secs.map(|t| format!("{t} s")).unwrap_or_else(|| "wait for a choice".into()));
    println!("Preselected:    {}", c.default_entry.as_deref().unwrap_or("the first entry"));
    println!("Text mode:      {}", if c.text_mode { "on" } else { "off" });
    println!("Theme:          {}", c.theme.as_deref().unwrap_or("NyBoot (built in)"));
    println!("Folder depth:   {}", c.depth.map(|d| d.to_string()).unwrap_or_else(|| "3 (default)".into()));
}

fn config(a: ConfigArgs) -> Res {
    use nyboot::config;
    let t = resolve(&a.target, &a.common, true)?;
    need_root(&t, "Reading a drive")?;
    let mut od = drive::open(&t, true)?;
    let mut c = config::load(&od)?;
    let changing = a.timeout.is_some() || a.no_timeout || a.default.is_some() || a.clear_default || a.text_mode.is_some() || a.depth.is_some() || a.theme.is_some() || a.clear_theme || a.reset;
    if !changing {
        show_config(&c, a.common.json);
        return Ok(0);
    }
    if a.reset {
        c = config::MenuConfig::default();
    }
    if let Some(v) = a.timeout {
        c.timeout_secs = Some(v);
    }
    if a.no_timeout {
        c.timeout_secs = None;
    }
    if let Some(v) = a.default {
        c.default_entry = Some(v);
    }
    if a.clear_default {
        c.default_entry = None;
    }
    if let Some(v) = &a.text_mode {
        c.text_mode = match v.to_ascii_lowercase().as_str() {
            "on" | "yes" | "true" => true,
            "off" | "no" | "false" => false,
            _ => return Err(NyError::invalid("--text-mode takes on or off.")),
        };
    }
    if let Some(v) = a.depth {
        c.depth = Some(v);
    }
    if let Some(v) = a.theme {
        c.theme = Some(v);
    }
    if a.clear_theme {
        c.theme = None;
    }
    config::save(&mut od, &c)?;
    if !a.common.json {
        eprintln!("Saved. These apply the next time the drive starts.");
    }
    show_config(&config::load(&od)?, a.common.json);
    Ok(0)
}

fn theme(a: ThemeArgs) -> Res {
    use nyboot::config;
    let t = resolve(&a.target, &a.common, true)?;
    need_root(&t, "Writing to a drive")?;
    let name = match &a.name {
        Some(n) => n.clone(),
        None => a.folder.canonicalize().ok().and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())).unwrap_or_default(),
    };
    let mut od = drive::open(&t, true)?;
    let n = config::install_theme(&mut od, &a.folder, &name)?;
    if !a.keep_current {
        let mut c = config::load(&od)?;
        c.theme = Some(name.clone());
        config::save(&mut od, &c)?;
    }
    if a.common.json {
        println!("{}", serde_json::json!({ "theme": name, "files": n, "in_use": !a.keep_current }));
    } else if a.keep_current {
        println!("Theme \"{name}\" copied ({n} files). Use it with: nyforge boot config <drive> --theme \"{name}\"");
    } else {
        println!("Theme \"{name}\" copied ({n} files) and selected. It shows the next time the drive starts.");
    }
    Ok(0)
}

fn params(a: ParamsArgs) -> Res {
    use nyboot::config;
    let t = resolve(&a.target, &a.common, true)?;
    need_root(&t, "Writing to a drive")?;
    let mut od = drive::open(&t, true)?;
    if a.options.is_none() && !a.clear {
        let p = config::get_params(&od, &a.iso)?;
        if a.common.json {
            println!("{}", serde_json::json!({ "params": p.unwrap_or_default() }));
        } else {
            match p {
                Some(p) => println!("{p}"),
                None => println!("(no extra options)"),
            }
        }
        return Ok(0);
    }
    config::set_params(&mut od, &a.iso, if a.clear { None } else { a.options.as_deref() })?;
    if !a.common.json {
        eprintln!("{}", if a.clear { "Removed the extra options." } else { "Saved. They are added the next time this ISO starts." });
    }
    Ok(0)
}
