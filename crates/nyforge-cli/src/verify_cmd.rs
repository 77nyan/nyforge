//! `nyforge verify` (read a drive back and compare it with its image) and `nyforge checksum` (check a download).

use crate::ui::Ui;
use crate::{install_ctrlc, is_root, rerun_hint, Res};
use clap::Args;
use nyforge_core::checksum::{self, Algo, Expected};
use nyforge_core::device::{self, TargetPolicy};
use nyforge_core::error::{ErrorKind, NyError};
use nyforge_core::format::{human_duration, human_size, human_speed};
use nyforge_core::progress::Event;
use nyforge_core::verify::{verify_image, VerifyOptions};
use nyforge_core::winmedia::{verify_windows_media, Unattend, WindowsOptions};
use nyforge_core::writer::Target;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

#[derive(Args)]
pub struct VerifyArgs {
    /// The image file that was written (or the Windows ISO, with --windows)
    pub image: String,
    /// The drive to read back (e.g. /dev/sdb, /dev/disk4, \\.\PhysicalDrive2), or the image file it was written to
    pub target: String,
    /// The drive holds a Windows installer made by `nyforge windows`: compare every file with the ISO
    #[arg(long)]
    pub windows: bool,
    /// With --windows: the installer was made with --bypass-checks
    #[arg(long, requires = "windows")]
    pub bypass_checks: bool,
    /// With --windows: the installer was made with --uefi-only
    #[arg(long, requires = "windows")]
    pub uefi_only: bool,
    /// Buffered I/O instead of direct I/O
    #[arg(long)]
    pub no_direct: bool,
    #[arg(long)]
    pub json: bool,
}

/// Compare one finished write with its image, printing progress. Used by `verify` and by `--verify` on the writing commands.
pub fn verify_after(image: &str, target: &Target, direct: bool, json: bool, ui: &mut Ui, cancel: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Result<(), NyError> {
    let source = nyforge_core::image::open(image)?;
    let opts = VerifyOptions { direct_io: direct, ..Default::default() };
    let mut emit = |ev: Event| {
        if json {
            crate::print_event(&ev);
        } else {
            ui.render(&ev);
        }
    };
    let r = verify_image(source, target, &opts, cancel, &mut emit);
    ui.finish_line();
    let rep = r?;
    if !json {
        let secs = rep.elapsed.as_secs_f64();
        eprintln!(
            "{} Verified {} on {}: it matches the image ({}, {}).",
            ui.green("✔"),
            human_size(rep.bytes),
            target.display,
            human_duration(secs),
            human_speed(if secs > 0.0 { rep.bytes as f64 / secs } else { 0.0 })
        );
    }
    Ok(())
}

pub fn verify_windows_after(iso: &Path, target: &Target, opts: &WindowsOptions, json: bool, ui: &mut Ui, cancel: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Result<(), NyError> {
    let mut emit = |ev: Event| {
        if json {
            crate::print_event(&ev);
        } else {
            ui.render(&ev);
        }
    };
    let r = verify_windows_media(iso, target, opts, cancel, &mut emit);
    ui.finish_line();
    let rep = r?;
    if !json {
        eprintln!("{} Verified {} files ({}) on {}: they match the ISO.", ui.green("✔"), rep.files, human_size(rep.bytes), target.display);
    }
    Ok(())
}

pub fn run(a: VerifyArgs) -> Res {
    let policy = TargetPolicy { allow_internal: true, allow_virtual: true };
    let resolved = device::resolve_target(&a.target, policy)?;
    let target = Target::from_resolved(resolved);
    if target.drive.is_some() && !is_root() {
        return Err(NyError::new(ErrorKind::Permission, "Reading a whole drive needs administrator rights.")
            .with_hint(rerun_hint(&format!("nyforge verify {} {}", a.image, a.target))));
    }
    let cancel = install_ctrlc();
    let mut ui = Ui::new();
    if a.windows {
        let opts = WindowsOptions { bios: !a.uefi_only, unattend: Unattend { bypass_hardware_checks: a.bypass_checks }, ..Default::default() };
        verify_windows_after(Path::new(&a.image), &target, &opts, a.json, &mut ui, cancel)?;
    } else {
        verify_after(&a.image, &target, !a.no_direct, a.json, &mut ui, cancel)?;
    }
    Ok(0)
}

// ------------------------------------------------------------------------------------------------ checksum

#[derive(Args)]
pub struct ChecksumArgs {
    /// The file to check (the download, as it is: an .iso, or an .iso.xz)
    pub file: PathBuf,
    /// The checksum to compare with: the hash itself, or a checksum file (.sha256, SHA256SUMS, …).
    /// Without it, a checksum file next to the download is used when there is one.
    #[arg(long, value_name = "HASH|FILE")]
    pub expect: Option<String>,
    /// Which hash to print when there is nothing to compare with (sha256, sha1, md5)
    #[arg(long, value_name = "NAME", default_value = "sha256")]
    pub algo: String,
    #[arg(long)]
    pub json: bool,
}

/// What `--expect` means: a file to read, or the hash itself.
fn expectation(spec: &str, file: &Path) -> Result<(Expected, Option<PathBuf>), NyError> {
    let name = file.file_name().map(|n| n.to_string_lossy().into_owned());
    let p = Path::new(spec);
    if p.is_file() {
        let text = std::fs::read_to_string(p).map_err(|e| NyError::from_io(&e, &format!("reading {spec}")))?;
        let e = checksum::parse_expected(&text, name.as_deref())
            .ok_or_else(|| NyError::invalid(format!("{spec} has no checksum for {}.", name.as_deref().unwrap_or("this file"))))?;
        return Ok((e, Some(p.to_path_buf())));
    }
    checksum::parse_expected(spec, name.as_deref())
        .map(|e| (e, None))
        .ok_or_else(|| NyError::invalid("That is not a SHA-256, SHA-1 or MD5 checksum.").with_hint("A SHA-256 is 64 characters of 0-9 and a-f; SHA-1 has 40, MD5 has 32."))
}

pub fn run_checksum(a: ChecksumArgs) -> Res {
    let cancel = install_ctrlc();
    let ui = Ui::new();
    let (expected, from) = match &a.expect {
        Some(spec) => {
            let (e, f) = expectation(spec, &a.file)?;
            (Some(e), f)
        }
        None => match checksum::find_beside(&a.file) {
            Some((f, e)) => (Some(e), Some(f)),
            None => (None, None),
        },
    };
    let tty = std::io::IsTerminal::is_terminal(&std::io::stderr()) && !a.json;
    let mut last = 0u64;
    let mut progress = |done: u64, total: u64| {
        if tty && total > 0 {
            let pct = done * 100 / total;
            if pct != last {
                last = pct;
                eprint!("\r{} {pct:>3}%", ui.dim("Checking"));
            }
        }
    };
    let Some(expected) = expected else {
        let algo = Algo::from_name(&a.algo).ok_or_else(|| NyError::invalid(format!("\"{}\" is not sha256, sha1 or md5.", a.algo)))?;
        let hex = checksum::hash_file(&a.file, algo, &cancel, &mut progress)?;
        if tty {
            eprint!("\r\x1b[2K");
        }
        if a.json {
            println!("{}", serde_json::json!({ "checksum": { "algo": algo.name(), "actual": hex, "expected": null, "matches": null } }));
        } else {
            println!("{hex}  {}", a.file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
            eprintln!("{}", ui.dim("No checksum to compare with: pass --expect <hash or checksum file>."));
        }
        return Ok(0);
    };
    let res = checksum::check_file(&a.file, &expected, &cancel, &mut progress);
    if tty {
        eprint!("\r\x1b[2K");
    }
    let r = res?;
    if cancel.load(Ordering::Relaxed) {
        return Err(NyError::cancelled());
    }
    if a.json {
        println!(
            "{}",
            serde_json::json!({ "checksum": { "algo": r.algo.name(), "actual": r.actual, "expected": r.expected, "matches": r.matches, "from": from.map(|p| p.display().to_string()) } })
        );
        return Ok(if r.matches { 0 } else { 1 });
    }
    if r.matches {
        eprintln!("{} {} matches{}.", ui.green("✔"), r.algo.name(), from.map(|p| format!(" ({})", p.display())).unwrap_or_default());
        println!("{}", r.actual);
        Ok(0)
    } else {
        Err(checksum::mismatch_error(&r))
    }
}
