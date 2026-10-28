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
    #[command(flatten)]
    pub answers: AnswerArgs,
    /// UEFI only: skip the legacy BIOS boot code
    #[arg(long)]
    pub uefi_only: bool,
    /// Drive layout: auto (FAT32 unless the files need NTFS), fat32 (UEFI and legacy BIOS), ntfs (NTFS + UEFI:NTFS, UEFI only; handles install.esd over 4 GB)
    #[arg(long, value_name = "LAYOUT", default_value = "auto")]
    pub layout: nyforge_core::winntfs::WinLayout,
    /// Do not ask for confirmation (required when not run from a terminal)
    #[arg(short = 'y', long)]
    pub yes: bool,
    /// Allow fixed internal disks (never the system disk)
    #[arg(long)]
    pub allow_internal: bool,
    #[arg(long)]
    pub allow_virtual: bool,
    /// When it is made, read the drive back and compare every file with the ISO
    #[arg(long)]
    pub verify: bool,
    /// When it is made (and verified), detach the drive so it can be unplugged right away (Linux and macOS)
    #[arg(long)]
    pub eject: bool,
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
        // An image file that already exists keeps its size (that is also how the app's test drives work).
        None if t.drive.is_none() => std::fs::metadata(&t.path).ok().filter(|m| m.is_file() && m.len() > 0).map(|m| m.len()),
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
        unattend: a.answers.to_unattend()?,
        layout: a.layout,
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
        make_windows_media(&a.iso, &t, &opts, cancel.clone(), &mut emit)
    };
    ui.finish_line();
    let rep = res?;
    if a.verify || crate::verify_requested() {
        crate::verify_cmd::verify_windows_after(&a.iso, &t, &opts, a.json, &mut ui, cancel.clone())?;
    }
    if !a.json {
        eprintln!("{} Windows installer ready on {} ({}, volume {}).", ui.green("✔"), t.display, human_size(rep.bytes), rep.label);
        for n in &rep.notes {
            eprintln!("  - {n}");
        }
        eprintln!("  Boot the target computer from this drive. (Not yet tried on real Windows hardware: tell us how it goes.)");
    }
    if a.eject {
        crate::eject_note(&t, a.json, &ui);
    }
    Ok(0)
}

/// The setup answers, shared by `windows` (to write them) and `verify --windows` (to expect them).
#[derive(clap::Args, Clone, Default)]
pub struct AnswerArgs {
    /// Windows 11: skip the TPM, Secure Boot, RAM, CPU and storage checks in Setup
    #[arg(long)]
    pub bypass_checks: bool,
    /// Do not ask for a Microsoft account when Windows starts for the first time
    #[arg(long)]
    pub skip_online_account: bool,
    /// Make this local administrator account (no password), so Windows starts without a Microsoft account
    #[arg(long, value_name = "NAME")]
    pub local_account: Option<String>,
    /// Do not ask the privacy and data-sharing questions when Windows starts for the first time
    #[arg(long)]
    pub skip_privacy_questions: bool,
    /// Do not turn on automatic BitLocker device encryption
    #[arg(long)]
    pub disable_bitlocker: bool,
    /// Language and region of the new system: a tag like en-US, or "auto" for this computer's
    #[arg(long, value_name = "TAG")]
    pub locale: Option<String>,
}

impl AnswerArgs {
    pub fn to_unattend(&self) -> Result<Unattend, NyError> {
        if let Some(n) = &self.local_account {
            nyforge_core::winmedia::check_account_name(n)?;
        }
        let locale = match self.locale.as_deref() {
            None => None,
            Some("auto") => {
                let found = nyforge_core::winmedia::detect_locale();
                if found.is_none() {
                    // Not worth stopping for: the installer is made without a language setting, and Windows asks for it.
                    eprintln!("Could not tell this computer's language and region, so the language is not preset (Windows will ask).");
                }
                found
            }
            Some(t) => {
                nyforge_core::winmedia::check_locale(t)?;
                Some(t.to_string())
            }
        };
        Ok(Unattend {
            bypass_hardware_checks: self.bypass_checks,
            skip_online_account: self.skip_online_account,
            local_account: self.local_account.clone(),
            skip_privacy_questions: self.skip_privacy_questions,
            disable_bitlocker: self.disable_bitlocker,
            locale,
        })
    }
}
