//! `nyforge format` — wipe and re-format a USB drive or SD card.
//!
//! This command has no `--allow-internal` or `--allow-virtual`: it only ever works on removable
//! USB drives and SD cards (or an image file), and the engine enforces that again on its own.

use crate::ui::Ui;
use crate::{confirm_erase, install_ctrlc, is_root, Res};
use clap::Args;
use nyforge_core::device::Drive;
use nyforge_core::error::{ErrorKind, NyError};
use nyforge_core::format::{human_size, parse_size};
use nyforge_core::progress::Event;
use nyforge_core::usbformat::{self, capacity::CapacityTest, FormatOptions, PartSpec, Scheme, Wipe};
use nyforge_core::writer::Target;
use std::io::{BufRead, IsTerminal, Write};

#[derive(Args)]
pub struct FormatArgs {
    /// USB drive or SD card to format (e.g. /dev/sdb), or an image file. Leave out to pick from a list.
    pub target: Option<String>,
    /// Show the drives that can be formatted, then stop
    #[arg(long)]
    pub list: bool,
    /// Filesystem: fat32, exfat, fat16, ntfs, ext4 [default: FAT32 up to 32 GB, exFAT beyond]
    #[arg(long, value_name = "FS")]
    pub fs: Option<String>,
    /// Volume name
    #[arg(short, long, default_value = "", value_name = "NAME")]
    pub label: String,
    /// Partition table: mbr, gpt or none (filesystem right at the start, for cameras and TVs) [default: mbr, gpt above 2 TB]
    #[arg(long, value_name = "SCHEME")]
    pub scheme: Option<String>,
    /// Make several partitions instead: fs[:size[:label[:cluster]]], repeat for each, e.g. --partition fat32:16G:WIN --partition ext4::LINUX
    #[arg(long = "partition", value_name = "SPEC")]
    pub partitions: Vec<String>,
    /// How to erase the old contents: quick (partition tables only), zero (every byte), random (every byte)
    #[arg(long, default_value = "quick", value_name = "MODE")]
    pub wipe: String,
    /// Check that the drive really holds what it claims (finds counterfeit sticks): quick (default when given) or full
    #[arg(long, value_name = "MODE", num_args = 0..=1, default_missing_value = "quick")]
    pub test_capacity: Option<String>,
    /// If the capacity test finds a fake, format only the part that works (needs --test-capacity full)
    #[arg(long)]
    pub fit_real_capacity: bool,
    /// Cluster size, e.g. 32K (default: chosen for the size)
    #[arg(long, value_name = "SIZE")]
    pub cluster_size: Option<String>,
    /// Leave this much of the end of the drive unpartitioned, e.g. 2G
    #[arg(long, value_name = "SIZE")]
    pub reserve: Option<String>,
    /// Size of the image file when the target is a file, e.g. 4G
    #[arg(long, value_name = "SIZE")]
    pub size: Option<String>,
    /// ext4: who owns the top folder, as UID:GID (default: the user who ran sudo)
    #[arg(long, value_name = "UID:GID")]
    pub owner: Option<String>,
    /// Do not ask for confirmation
    #[arg(short = 'y', long)]
    pub yes: bool,
    /// One JSON event per line
    #[arg(long)]
    pub json: bool,
}

fn bad<T>(msg: String) -> Result<T, NyError> {
    Err(NyError::invalid(msg))
}

fn parse_owner(s: &str) -> Result<(u32, u32), NyError> {
    let (u, g) = s.split_once(':').ok_or_else(|| NyError::invalid("--owner is UID:GID, for example 1000:1000."))?;
    match (u.parse(), g.parse()) {
        (Ok(u), Ok(g)) => Ok((u, g)),
        _ => bad("--owner is UID:GID, for example 1000:1000.".into()),
    }
}

/// The person who started this with sudo/pkexec, so an ext4 stick is theirs and not root's.
fn invoking_user() -> Option<(u32, u32)> {
    let uid = std::env::var("SUDO_UID").or_else(|_| std::env::var("PKEXEC_UID")).ok()?.parse::<u32>().ok()?;
    let gid = std::env::var("SUDO_GID").ok().and_then(|g| g.parse().ok()).unwrap_or(uid);
    (uid != 0).then_some((uid, gid))
}

fn drive_table(ui: &Ui, drives: &[Drive], numbered: bool) {
    let mut head = vec![];
    if numbered {
        head.push("#".to_string());
    }
    head.extend(["DRIVE", "SIZE", "TYPE", "NAME", "CONTAINS"].map(String::from));
    let mut rows = vec![head];
    for (i, d) in drives.iter().enumerate() {
        let mut r = vec![];
        if numbered {
            r.push((i + 1).to_string());
        }
        r.extend([d.path.clone(), human_size(d.size), d.bus.label().into(), d.display_name(), d.contents_summary()]);
        rows.push(r);
    }
    ui.table(&rows);
}

fn pick_interactively(ui: &Ui) -> Result<String, NyError> {
    let drives = usbformat::list_formattable()?;
    if drives.is_empty() {
        return Err(NyError::new(ErrorKind::InvalidInput, "No USB drive or SD card found.").with_hint("Plug one in and try again. Internal disks are never offered."));
    }
    if !std::io::stdin().is_terminal() {
        return Err(NyError::invalid("Say which drive to format: nyforge format /dev/sdb").with_hint("`nyforge format --list` shows the drives that can be formatted."));
    }
    drive_table(ui, &drives, true);
    eprint!("\nWhich drive? [1-{}]: ", drives.len());
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).ok();
    match line.trim().parse::<usize>() {
        Ok(n) if (1..=drives.len()).contains(&n) => Ok(drives[n - 1].path.clone()),
        _ => Err(NyError::cancelled().with_hint("Nothing was written.")),
    }
}

pub fn run(a: FormatArgs) -> Res {
    let ui = Ui::new();
    if a.list {
        let drives = usbformat::list_formattable()?;
        if a.json {
            println!("{}", serde_json::to_string_pretty(&drives).unwrap());
        } else if drives.is_empty() {
            println!("No USB drive or SD card found. (Internal disks are never listed.)");
        } else {
            drive_table(&ui, &drives, false);
        }
        return Ok(0);
    }

    // ---- read every option before anything is touched
    let spec = match &a.target {
        Some(t) => t.clone(),
        None => {
            if a.json {
                return bad("--json needs the drive on the command line.".into());
            }
            pick_interactively(&ui)?
        }
    };
    let target = usbformat::resolve_target(&spec)?;
    if target.drive.is_some() && !is_root() {
        return Err(NyError::new(ErrorKind::Permission, "Formatting a drive needs administrator rights.")
            .with_hint(nyforge_core::sys::elevation_hint()));
    }
    let file_size = match &a.size {
        Some(s) => Some(parse_size(s).ok_or_else(|| NyError::invalid(format!("\"{s}\" is not a size like 4G or 512M.")))?),
        None => None,
    };
    let disk_bytes = target.capacity.or(file_size).unwrap_or(0);

    let parts: Vec<PartSpec> = if a.partitions.is_empty() {
        let fs = match &a.fs {
            Some(f) => f.parse().map_err(NyError::invalid)?,
            None => usbformat::auto_fs(disk_bytes),
        };
        let cluster = match &a.cluster_size {
            Some(c) => Some(parse_size(c).ok_or_else(|| NyError::invalid(format!("\"{c}\" is not a cluster size like 32K.")))?),
            None => None,
        };
        vec![PartSpec { fs, size: None, label: a.label.clone(), cluster }]
    } else {
        if a.fs.is_some() || !a.label.is_empty() || a.cluster_size.is_some() {
            return bad("--partition describes everything itself (fs, size, label, cluster): leave out --fs, --label and --cluster-size.".into());
        }
        a.partitions.iter().map(|p| p.parse::<PartSpec>().map_err(NyError::invalid)).collect::<Result<_, _>>()?
    };
    let scheme: Scheme = match a.scheme.as_deref() {
        None | Some("auto") => usbformat::auto_scheme(disk_bytes),
        Some(s) => s.parse().map_err(NyError::invalid)?,
    };
    let wipe: Wipe = a.wipe.parse().map_err(NyError::invalid)?;
    let capacity_test = match a.test_capacity.as_deref() {
        None | Some("off") => CapacityTest::Off,
        Some("quick") => CapacityTest::Quick,
        Some("full") => CapacityTest::Full,
        Some(o) => return bad(format!("\"{o}\" is not a capacity test mode (quick, full).")),
    };
    let reserve_bytes = match &a.reserve {
        Some(r) => parse_size(r).ok_or_else(|| NyError::invalid(format!("\"{r}\" is not a size like 2G.")))?,
        None => 0,
    };
    let owner = match &a.owner {
        Some(o) => Some(parse_owner(o)?),
        None => invoking_user(),
    };
    let opts = FormatOptions { parts, scheme, wipe, capacity_test, fit_real_capacity: a.fit_real_capacity, reserve_bytes, file_size, unmount: true, owner };

    // ---- confirm
    if let Some(d) = &target.drive {
        if !a.yes {
            if a.json {
                return Err(NyError::new(ErrorKind::InvalidInput, "--json needs --yes: there is nobody to ask."));
            }
            let what = opts.parts.iter().map(|p| p.fs.name()).collect::<Vec<_>>().join(" + ");
            confirm_erase(&[d], &format!("a fresh {what}"), &ui)?;
        }
    }

    // ---- go
    let cancel = install_ctrlc();
    let mut ui = ui;
    let res = {
        let json = a.json;
        let ui_ref = &mut ui;
        let mut emit = move |ev: Event| {
            if json {
                crate::print_event(&ev);
            } else {
                ui_ref.render(&ev);
            }
        };
        let t: &Target = &target;
        usbformat::format(t, &opts, cancel, &mut emit)
    };
    ui.finish_line();
    let rep = res?;
    if a.json {
        let mut v = serde_json::to_value(&rep).unwrap();
        v["event"] = "format_report".into();
        println!("{v}");
    } else {
        eprintln!("{} Formatted {} ({}, {})", ui.green("✔"), rep.target, human_size(rep.usable_bytes), rep.scheme.name());
        for p in &rep.partitions {
            let name = if p.label.is_empty() { "(no label)".to_string() } else { p.label.clone() };
            eprintln!("  {}  {:<6} {:>9}  {}", p.number, p.fs.name(), human_size(p.size_bytes), name);
        }
        if let Some(c) = &rep.capacity {
            if c.genuine {
                eprintln!("  Capacity test ({:?}): all {} test blocks came back correct.", c.mode, c.tested_bytes.div_ceil(if c.mode == CapacityTest::Full { 1 << 20 } else { 4096 }).max(1));
            }
        }
        for n in &rep.notes {
            eprintln!("  {}", ui.dim(n));
        }
    }
    Ok(0)
}
