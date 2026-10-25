//! `nyforge windows` — Windows install media from an ISO.

use crate::ui::Ui;
use crate::{confirm_erase, install_ctrlc, is_root, Res};
use clap::Args;
use nyforge_core::device::{self, Drive, TargetPolicy};
use nyforge_core::error::{ErrorKind, NyError};
use nyforge_core::format::{human_size, parse_size};
use nyforge_core::progress::Event;
use nyforge_core::winmedia::{make_windows_media, Unattend, WindowsOptions};
use nyforge_core::writer::Target;
use std::path::PathBuf;

#[derive(Args)]
pub struct WindowsArgs {
    /// Windows ISO file
    pub iso: PathBuf,
    /// USB drive (e.g. /dev/sdb) or an image file to create
    pub target: String,
    /// Size of the image file when the target is a file, e.g. 16G
    #[arg(long, value_name = "SIZE")]
    pub size: Option<String>,
    /// Volume name (default: taken from the ISO)
    #[arg(long)]
    pub label: Option<String>,
    /// Windows 11: skip the TPM, Secure Boot, RAM, CPU and storage checks in Setup
    #[arg(long)]
    pub bypass_checks: bool,
    /// UEFI only: skip the legacy BIOS boot code
    #[arg(long)]
    pub uefi_only: bool,
    /// Do not ask for confirmation (required when not run from a terminal)
    #[arg(short = 'y', long)]
    pub yes: bool,
    /// Allow fixed internal disks (never the system disk)
    #[arg(long)]
    pub allow_internal: bool,
    #[arg(long)]
    pub allow_virtual: bool,
    #[arg(long)]
    pub json: bool,
}

pub fn run(a: WindowsArgs) -> Res {
    let policy = TargetPolicy { allow_internal: a.allow_internal, allow_virtual: a.allow_virtual };
    let t = Target::from_resolved(device::resolve_target(&a.target, policy)?);
    if t.drive.is_some() && !is_root() {
        return Err(NyError::new(ErrorKind::Permission, "Writing to a drive needs administrator rights.")
            .with_hint(nyforge_core::sys::elevation_hint()));
    }
    let file_size = match &a.size {
        Some(s) => Some(parse_size(s).ok_or_else(|| NyError::invalid(format!("\"{s}\" is not a size like 16G or 512M.")))?),
        None => None,
    };
    let drives: Vec<&Drive> = t.drive.iter().collect();
    if !drives.is_empty() && !a.yes {
        if a.json {
            return Err(NyError::new(ErrorKind::InvalidInput, "--json needs --yes: there is nobody to ask."));
        }
        let name = a.iso.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        confirm_erase(&drives, &name, &Ui::new())?;
    }
    let opts = WindowsOptions {
        label: a.label.clone(),
        file_size,
        bios: !a.uefi_only,
        unattend: Unattend { bypass_hardware_checks: a.bypass_checks },
        ..Default::default()
    };
    let cancel = install_ctrlc();
    let mut ui = Ui::new();
    let json = a.json;
    let res = {
        let mut emit = |ev: Event| {
            if json {
                crate::print_event(&ev);
            } else {
                ui.render(&ev);
            }
        };
        make_windows_media(&a.iso, &t, &opts, cancel, &mut emit)
    };
    ui.finish_line();
    let rep = res?;
    if !a.json {
        eprintln!("{} Windows installer ready on {} ({}, volume {}).", ui.green("✔"), t.display, human_size(rep.bytes), rep.label);
        for n in &rep.notes {
            eprintln!("  - {n}");
        }
        eprintln!("  Boot the target computer from this drive. (Not yet tried on real Windows hardware: tell us how it goes.)");
    }
    Ok(0)
}
