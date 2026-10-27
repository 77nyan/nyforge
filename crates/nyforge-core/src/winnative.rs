//! Windows' own storage tools (`diskpart`), for the jobs where talking to the raw disk is the fragile way.
//!
//! Some USB sticks (and some USB controllers or driver setups) drop off the bus when a program writes to
//! `\\.\PhysicalDriveN` directly; Windows' own formatter does not trigger that, because it goes through the volume
//! and partition stack. balenaEtcher cleans a disk with `diskpart` before it writes, for the same reason. This module
//! builds and runs those `diskpart` scripts:
//!
//! * [`clean_script`]: remove every partition (and make the disk writable and online), so the raw writes that follow
//!   do not hit a live partition table;
//! * [`format_script`]: clean, partition and format in one go: what a "format" button in Windows does. NyFormat uses
//!   it first on Windows for plain quick formats.
//!
//! Everything that can be decided without Windows (the scripts, whether a request can be done this way) is a plain
//! function, tested on every platform.

use crate::device::{Bus, Drive, Safety};
use crate::error::{ErrorKind, NyError, Result};
use crate::usbformat::{Fs, FormatOptions, PartPlan, Scheme, Wipe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;

/// One partition for `diskpart`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativePart {
    pub fs: Fs,
    /// In MiB; `None` = everything that is left (the last partition only).
    pub size_mib: Option<u64>,
    pub label: String,
    /// Allocation unit in bytes.
    pub cluster: Option<u64>,
}

/// `diskpart`'s name for a filesystem it can make. Not the ext family.
fn fs_word(fs: Fs) -> Option<&'static str> {
    match fs {
        Fs::Fat16 => Some("fat"),
        Fs::Fat32 => Some("fat32"),
        Fs::ExFat => Some("exfat"),
        Fs::Ntfs => Some("ntfs"),
        Fs::Ext2 | Fs::Ext3 | Fs::Ext4 => None,
    }
}

/// Make the disk writable and online, then drop every partition. `noerr` keeps a disk that is already online and
/// writable from stopping the script.
pub fn clean_script(disk: u32) -> String {
    format!("select disk {disk}\r\nattributes disk clear readonly noerr\r\nonline disk noerr\r\nclean\r\nrescan\r\nexit\r\n")
}

/// Clean the disk, give it a partition table and create and format every partition, assigning drive letters.
pub fn format_script(disk: u32, gpt: bool, parts: &[NativePart]) -> std::result::Result<String, String> {
    if parts.is_empty() {
        return Err("Nothing to create.".into());
    }
    let mut s = format!(
        "select disk {disk}\r\nattributes disk clear readonly noerr\r\nonline disk noerr\r\nclean\r\nconvert {}\r\n",
        if gpt { "gpt" } else { "mbr" }
    );
    for (i, p) in parts.iter().enumerate() {
        let word = fs_word(p.fs).ok_or_else(|| format!("Windows cannot make {} itself.", p.fs.name()))?;
        if p.size_mib.is_none() && i + 1 != parts.len() {
            return Err("Only the last partition can take the rest of the drive.".into());
        }
        if p.label.chars().any(|c| c == '"' || c.is_control()) {
            return Err("That volume name has a character diskpart cannot take.".into());
        }
        match p.size_mib {
            Some(mb) => s.push_str(&format!("create partition primary size={mb}\r\n")),
            None => s.push_str("create partition primary\r\n"),
        }
        s.push_str(&format!("format fs={word} quick"));
        if !p.label.is_empty() {
            s.push_str(&format!(" label=\"{}\"", p.label));
        }
        if let Some(c) = p.cluster {
            s.push_str(&format!(" unit={c}"));
        }
        s.push_str("\r\nassign\r\n");
    }
    s.push_str("exit\r\n");
    Ok(s)
}

/// Can this format be done by Windows' own tools, and with which partitions? `Err` says why not, for the log.
/// Only the plain job qualifies: a quick format with a partition table. Wiping, capacity tests, spare space at the
/// end and the ext family stay with NyForge's own writer, which is where those features live.
pub fn eligible(opts: &FormatOptions, plan: &[PartPlan]) -> std::result::Result<Vec<NativePart>, String> {
    if opts.scheme == Scheme::None {
        return Err("a drive without a partition table".into());
    }
    if opts.wipe != Wipe::Quick || opts.capacity_test != crate::usbformat::capacity::CapacityTest::Off || opts.fit_real_capacity {
        return Err("a wipe or a capacity test was asked for".into());
    }
    if opts.reserve_bytes != 0 {
        return Err("spare space at the end of the drive was asked for".into());
    }
    let mut out = vec![];
    for (i, p) in plan.iter().enumerate() {
        if fs_word(p.spec.fs).is_none() {
            return Err(format!("{} is not something Windows makes itself", p.spec.fs.name()));
        }
        if p.spec.fs == Fs::Fat32 && p.bytes() > 32 * GIB {
            return Err("Windows refuses FAT32 above 32 GiB".into());
        }
        let last = i + 1 == plan.len();
        out.push(NativePart {
            fs: p.spec.fs,
            size_mib: if last && p.spec.size.is_none() { None } else { Some((p.bytes() / MIB).max(1)) },
            label: p.spec.label.clone(),
            cluster: p.spec.cluster,
        });
    }
    Ok(out)
}

/// The one rule for letting `diskpart` near a disk: removable USB or SD media that the drive list calls safe.
pub fn may_clean(d: &Drive) -> bool {
    d.safety == Safety::Safe && d.removable && matches!(d.bus, Bus::Usb | Bus::Sd) && !d.read_only
}

/// Run a `diskpart` script. Returns its output (in the system's language: read the exit status, not the words).
pub fn run_diskpart(script: &str, cancel: &AtomicBool, timeout: Duration) -> Result<String> {
    if !cfg!(windows) {
        return Err(NyError::new(ErrorKind::Unavailable, "Windows' own disk tools exist only on Windows."));
    }
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let file: PathBuf = std::env::temp_dir().join(format!("nyforge-diskpart-{}-{}.txt", std::process::id(), COUNTER.fetch_add(1, Ordering::SeqCst)));
    std::fs::write(&file, script).map_err(|e| NyError::from_io(&e, "writing the diskpart script"))?;
    let result = run_script_file(&file, cancel, timeout);
    let _ = std::fs::remove_file(&file);
    result
}

fn run_script_file(file: &std::path::Path, cancel: &AtomicBool, timeout: Duration) -> Result<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut cmd = Command::new("diskpart");
    cmd.arg("/s").arg(file).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd.spawn().map_err(|e| NyError::new(ErrorKind::Unavailable, format!("Could not start diskpart: {e}")))?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) => {}
            Err(e) => return Err(NyError::from_io(&e, "waiting for diskpart")),
        }
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(NyError::cancelled());
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(NyError::new(ErrorKind::Other, "Windows' disk tool did not answer in time."));
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let mut out = String::new();
    if let Some(mut so) = child.stdout.take() {
        let mut b = vec![];
        let _ = so.read_to_end(&mut b);
        out.push_str(&String::from_utf8_lossy(&b));
    }
    if status.success() {
        Ok(out)
    } else {
        let tail: String = out.lines().rev().filter(|l| !l.trim().is_empty()).take(3).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join(" ");
        Err(NyError::new(ErrorKind::Other, format!("Windows' disk tool reported an error: {}", tail.trim())))
    }
}

/// Ask Windows to clean the disk (every partition removed). Best effort: the callers carry on without it when it
/// fails, as they did before this existed.
pub fn clean_disk(drive: &Drive, cancel: &AtomicBool) -> Result<()> {
    if !may_clean(drive) {
        return Err(NyError::new(ErrorKind::Unsafe, format!("{} is not removable USB or SD media: NyForge will not clean it.", drive.path)));
    }
    let disk = crate::device::windows::disk_number(&drive.path).ok_or_else(|| NyError::other(format!("{} is not a Windows physical drive.", drive.path)))?;
    run_diskpart(&clean_script(disk), cancel, Duration::from_secs(45))?;
    // Let Windows finish tearing the old volumes down before anything else opens the disk.
    std::thread::sleep(Duration::from_millis(800));
    Ok(())
}

/// Clean, partition and format the whole drive with Windows' own tools.
pub fn format_disk(drive: &Drive, scheme: Scheme, parts: &[NativePart], cancel: &AtomicBool) -> Result<()> {
    if !may_clean(drive) {
        return Err(NyError::new(ErrorKind::Unsafe, format!("{} is not removable USB or SD media: NyForge will not format it.", drive.path)));
    }
    let disk = crate::device::windows::disk_number(&drive.path).ok_or_else(|| NyError::other(format!("{} is not a Windows physical drive.", drive.path)))?;
    let script = format_script(disk, scheme == Scheme::Gpt, parts).map_err(NyError::invalid)?;
    run_diskpart(&script, cancel, Duration::from_secs(300))?;
    std::thread::sleep(Duration::from_millis(500));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usbformat::{plan_layout, PartSpec};

    fn spec(fs: Fs, size: Option<u64>, label: &str) -> PartSpec {
        PartSpec { fs, size, label: label.into(), cluster: None }
    }

    #[test]
    fn the_clean_script_makes_the_disk_writable_before_it_cleans() {
        let s = clean_script(2);
        let lines: Vec<&str> = s.lines().collect();
        assert_eq!(lines[0], "select disk 2");
        let at = |w: &str| lines.iter().position(|l| l.starts_with(w)).unwrap();
        assert!(at("attributes disk clear readonly") < at("clean") && at("online disk") < at("clean"));
        assert!(s.contains("\r\n"), "diskpart scripts use Windows line ends");
        assert_eq!(lines.last(), Some(&"exit"));
    }

    #[test]
    fn a_format_script_cleans_partitions_formats_and_assigns() {
        let parts = vec![
            NativePart { fs: Fs::Fat32, size_mib: Some(16384), label: "WIN".into(), cluster: None },
            NativePart { fs: Fs::ExFat, size_mib: None, label: "DATA".into(), cluster: Some(32768) },
        ];
        let s = format_script(3, false, &parts).unwrap();
        let want = [
            "select disk 3",
            "attributes disk clear readonly noerr",
            "online disk noerr",
            "clean",
            "convert mbr",
            "create partition primary size=16384",
            "format fs=fat32 quick label=\"WIN\"",
            "assign",
            "create partition primary",
            "format fs=exfat quick label=\"DATA\" unit=32768",
            "assign",
            "exit",
        ];
        assert_eq!(s.lines().collect::<Vec<_>>(), want);
        assert!(format_script(3, true, &parts).unwrap().contains("convert gpt"));
    }

    #[test]
    fn a_format_script_refuses_what_diskpart_cannot_take() {
        let ext = [NativePart { fs: Fs::Ext4, size_mib: None, label: String::new(), cluster: None }];
        assert!(format_script(1, false, &ext).is_err());
        let quote = [NativePart { fs: Fs::Fat32, size_mib: None, label: "A\"B".into(), cluster: None }];
        assert!(format_script(1, false, &quote).is_err(), "a quote would end the label early");
        let two_rest = [
            NativePart { fs: Fs::Fat32, size_mib: None, label: String::new(), cluster: None },
            NativePart { fs: Fs::Fat32, size_mib: None, label: String::new(), cluster: None },
        ];
        assert!(format_script(1, false, &two_rest).is_err());
        assert!(format_script(1, false, &[]).is_err());
        let unnamed = [NativePart { fs: Fs::Ntfs, size_mib: None, label: String::new(), cluster: None }];
        assert!(!format_script(1, false, &unnamed).unwrap().contains("label="), "no label given: none is passed");
    }

    #[test]
    fn only_a_plain_quick_format_goes_to_windows() {
        let disk = 30 * GIB;
        let plain = |fs, scheme| FormatOptions { parts: vec![spec(fs, None, "STICK")], scheme, ..FormatOptions::default() };
        let plan = |o: &FormatOptions| plan_layout(disk, o.scheme, &o.parts, o.reserve_bytes).unwrap();

        let o = plain(Fs::Fat32, Scheme::Mbr);
        let parts = eligible(&o, &plan(&o)).unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!((parts[0].fs, parts[0].size_mib, parts[0].label.as_str()), (Fs::Fat32, None, "STICK"));

        let o = plain(Fs::ExFat, Scheme::Gpt);
        assert!(eligible(&o, &plan(&o)).is_ok());

        let o = plain(Fs::Fat32, Scheme::None);
        assert!(eligible(&o, &plan(&o)).is_err(), "no partition table: our own writer");
        let o = plain(Fs::Ext4, Scheme::Mbr);
        assert!(eligible(&o, &plan(&o)).is_err());
        let mut o = plain(Fs::Fat32, Scheme::Mbr);
        o.wipe = Wipe::Zero;
        assert!(eligible(&o, &plan(&o)).is_err(), "a wipe was asked for");
        let mut o = plain(Fs::Fat32, Scheme::Mbr);
        o.capacity_test = crate::usbformat::capacity::CapacityTest::Full;
        assert!(eligible(&o, &plan(&o)).is_err());
        let mut o = plain(Fs::Fat32, Scheme::Mbr);
        o.reserve_bytes = 100 * MIB;
        assert!(eligible(&o, &plan(&o)).is_err());

        // FAT32 above 32 GiB: Windows says no.
        let big = 64 * GIB;
        let o = plain(Fs::Fat32, Scheme::Mbr);
        let p = plan_layout(big, o.scheme, &o.parts, 0).unwrap();
        assert!(eligible(&o, &p).is_err());

        // Two partitions: the first keeps its size, the last takes the rest.
        let o = FormatOptions { parts: vec![spec(Fs::Fat32, Some(8 * GIB), "A"), spec(Fs::Ntfs, None, "B")], scheme: Scheme::Mbr, ..FormatOptions::default() };
        let parts = eligible(&o, &plan(&o)).unwrap();
        assert_eq!(parts[0].size_mib, Some(8192));
        assert_eq!(parts[1].size_mib, None);
    }

    #[test]
    fn only_removable_usb_media_may_be_cleaned() {
        use crate::device::Safety;
        let mut d = Drive {
            path: "\\\\.\\PhysicalDrive2".into(),
            vendor: String::new(),
            model: "SanDisk".into(),
            serial: None,
            size: 30 * GIB,
            sector_size: 512,
            bus: Bus::Usb,
            removable: true,
            read_only: false,
            safety: Safety::Safe,
            partitions: vec![],
        };
        assert!(may_clean(&d));
        d.safety = Safety::System;
        assert!(!may_clean(&d));
        d.safety = Safety::Internal;
        assert!(!may_clean(&d));
        d.safety = Safety::Safe;
        d.bus = Bus::Nvme;
        assert!(!may_clean(&d));
        d.bus = Bus::Usb;
        d.removable = false;
        assert!(!may_clean(&d));
        d.removable = true;
        d.read_only = true;
        assert!(!may_clean(&d));
        // And on anything but Windows nothing is run at all.
        if !cfg!(windows) {
            let never = AtomicBool::new(false);
            assert!(run_diskpart("exit\r\n", &never, Duration::from_secs(1)).is_err());
        }
    }
}
