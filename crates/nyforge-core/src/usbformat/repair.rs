//! Repair: look at what is left on a drive that will not format or shows the wrong size, and clean it out properly.
//!
//! A flashed ISO leaves a hybrid partition table, an ISO 9660 ("CD-ROM") image, sometimes a GPT whose backup copy is
//! in the middle of the drive; a half-finished format leaves stale filesystem signatures. A quick format only
//! touches the first and last megabyte, so those leftovers can survive and confuse the operating system (the drive
//! shows a few MB, or as a CD-ROM, or "needs to be formatted" forever). Repair reads the old layout first, tells you
//! what it found, clears the write-protect flag where the system allows it, and zeroes the start and the end of the
//! drive and of every old partition. The formatting that follows is the normal one.

use crate::error::{NyError, Result};
use crate::format::human_size;
use crate::nyboot::io::{FileIo, Io};
use crate::partition::{self, PartKind, SECTOR};
use crate::sys::{self, TargetKind};
use crate::writer::Target;
use std::sync::atomic::{AtomicBool, Ordering};

const MIB: u64 = 1 << 20;
/// How much is cleared at each end of the whole drive: room for every table, boot code and stray label.
const EDGE: u64 = 16 * MIB;

/// What the drive holds now, and what is wrong with it.
#[derive(Debug, Clone, Default)]
pub struct Diagnosis {
    /// Plain-words findings, most important first.
    pub findings: Vec<String>,
    /// Old partitions as (start byte, length in bytes).
    pub old_parts: Vec<(u64, u64)>,
}

fn io_for(target: &Target, len: u64, write: bool) -> Result<FileIo> {
    let block = target.kind == TargetKind::BlockDevice;
    let file = if write {
        if block {
            sys::open_rw_direct(&target.path)
        } else {
            std::fs::OpenOptions::new().read(true).write(true).open(&target.path)
        }
    } else {
        sys::open_read(&target.path, target.kind, block)
    }
    .map_err(|e| NyError::from_io(&e, &format!("opening {}", target.display)))?;
    Ok(FileIo::new(file, 0, len, if block { 4096 } else { 1 }))
}

/// Read the old layout. Reads only.
/// `len` is the size of the drive (or image file) in bytes.
pub fn diagnose(target: &Target, len: u64) -> Result<Diagnosis> {
    let io = io_for(target, len, false)?;
    let mut d = Diagnosis::default();
    if let Some(dr) = &target.drive {
        if dr.read_only {
            d.findings.push("The system reports the drive as write-protected. If it has a lock switch, slide it to unlocked; otherwise NyForge tries to clear the flag.".into());
        }
    }
    let mut head = vec![0u8; 64 * 1024];
    let have_head = io.read_at(&mut head, 0).is_ok();
    if have_head && &head[32769..32774] == b"CD001" {
        d.findings.push("An ISO 9660 (CD-ROM) image is on the drive: it holds a flashed ISO. This is why such a drive can show up as a few MB or as a disc.".into());
    }
    match partition::read_table(&io) {
        Ok(parts) => {
            let covered: u64 = parts.iter().map(|p| p.bytes()).sum();
            let kind = if parts.iter().any(|p| matches!(p.kind, PartKind::Gpt { .. })) {
                "GPT"
            } else if have_head && head[510] == 0x55 && head[511] == 0xAA {
                "MBR"
            } else {
                "no"
            };
            if parts.is_empty() {
                d.findings.push(if kind == "no" { "There is no partition table.".into() } else { format!("There is a {kind} partition table with no partitions in it.") });
            } else {
                d.findings.push(format!("{} {kind} partition table with {} partition{}, together {} of the drive's {}.", if kind == "MBR" { "An" } else { "A" }, parts.len(), if parts.len() == 1 { "" } else { "s" }, human_size(covered), human_size(len)));
                if covered + 64 * MIB < len {
                    d.findings.push(format!("{} of the drive is not in any partition, so Windows and Linux do not show it. Repair gives all of it back.", human_size(len - covered)));
                }
            }
            for p in &parts {
                let start = p.start * SECTOR;
                if start + 33_000 < len {
                    let mut pv = vec![0u8; 4096];
                    let at = (start + 32768) & !4095;
                    if at + 4096 <= len && io.read_at(&mut pv, at).is_ok() {
                        let off = ((start + 32768) - at) as usize;
                        if off + 5 <= pv.len() && &pv[off + 1..off + 6] == b"CD001" {
                            d.findings.push(format!("Partition {} is a read-only CD-ROM (ISO 9660 or U3) volume.", p.index));
                        }
                    }
                }
                if p.start * SECTOR + p.bytes() <= len {
                    d.old_parts.push((start, p.bytes()));
                }
            }
            if parts.iter().any(|p| matches!(p.kind, PartKind::Gpt { .. })) && len > 64 * 1024 {
                let mut tail = vec![0u8; 4096];
                if io.read_at(&mut tail, (len - 4096) & !4095).is_ok() && !tail.windows(8).any(|w| w == b"EFI PART") {
                    d.findings.push("The GPT's backup copy is not at the end of the drive (an image written to a bigger drive does this). Some systems then refuse the drive.".into());
                }
            }
        }
        Err(e) => d.findings.push(format!("The partition table is damaged ({}).", e.message)),
    }
    Ok(d)
}

/// Where to write zeros: both ends of the drive, and the start and end of every old partition. Sector-aligned and
/// inside the drive; overlapping pieces merged.
pub fn clean_ranges(len: u64, old_parts: &[(u64, u64)]) -> Vec<(u64, u64)> {
    let edge = EDGE.min(len / 2);
    let mut r: Vec<(u64, u64)> = vec![(0, edge), (len - edge, len)];
    for &(s, l) in old_parts {
        let e = MIB.min(l / 2);
        if e == 0 || s + l > len {
            continue;
        }
        r.push((s, s + e));
        r.push((s + l - e, s + l));
    }
    // whole 4 KiB blocks (raw handles want them), never past the end of the drive
    let mut r: Vec<(u64, u64)> = r.into_iter().map(|(a, b)| (a & !4095, ((b + 4095) & !4095).min(len))).filter(|(a, b)| b > a).collect();
    r.sort();
    let mut merged: Vec<(u64, u64)> = vec![];
    for (a, b) in r {
        match merged.last_mut() {
            Some(l) if a <= l.1 => l.1 = l.1.max(b),
            _ => merged.push((a, b)),
        }
    }
    merged
}

/// Zero the places a stale label can hide. The caller has already released the drive.
pub fn deep_clean(target: &Target, len: u64, diag: Option<&Diagnosis>, cancel: &AtomicBool) -> Result<()> {
    let io = io_for(target, len, true)?;
    let io = io.recovering(&target.path, target.drive.as_ref().filter(|_| target.kind == TargetKind::BlockDevice), sys::Reopen::RwDirect);
    for (a, b) in clean_ranges(len, diag.map(|d| d.old_parts.as_slice()).unwrap_or(&[])) {
        if cancel.load(Ordering::Relaxed) {
            return Err(NyError::cancelled());
        }
        io.zero(a, b - a).map_err(|e| NyError::from_io(&e, "clearing old data from the drive"))?;
    }
    io.sync().map_err(|e| NyError::from_io(&e, "flushing the drive"))?;
    Ok(())
}

/// Ask the system to make the drive writable again (Linux: the kernel's read-only flag). `false` when it would not.
/// A lock switch on the stick itself cannot be changed by software. Windows does this in its `diskpart` clean.
pub fn clear_read_only(path: &std::path::Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        const BLKROSET: libc::c_ulong = 0x125D;
        if let Ok(f) = std::fs::OpenOptions::new().read(true).open(path) {
            let zero: libc::c_int = 0;
            // SAFETY: BLKROSET takes a pointer to an int and only reads it.
            return unsafe { libc::ioctl(f.as_raw_fd(), BLKROSET, &zero) } == 0;
        }
        false
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ends_of_the_drive_and_of_every_old_partition_are_cleaned_without_overlap() {
        let len = 64 * MIB;
        let r = clean_ranges(len, &[(MIB, 40 * MIB), (41 * MIB, 20 * MIB)]);
        assert_eq!(r.first().unwrap().0, 0);
        assert_eq!(r.last().unwrap().1, len);
        for w in r.windows(2) {
            assert!(w[0].1 < w[1].0, "{:?} touches {:?}", w[0], w[1]);
        }
        assert!(r.iter().all(|&(a, b)| a % 4096 == 0 && (b % 4096 == 0 || b == len) && b <= len));
        // a partition in the middle of a big drive is cleaned at both of its ends
        let big = clean_ranges(1024 * MIB, &[(100 * MIB, 200 * MIB)]);
        assert!(big.iter().any(|&(a, b)| a <= 100 * MIB && b > 100 * MIB));
        assert!(big.iter().any(|&(a, b)| a < 300 * MIB && b >= 300 * MIB));
    }

    #[test]
    fn a_partition_beyond_the_drive_is_ignored() {
        let r = clean_ranges(32 * MIB, &[(30 * MIB, 20 * MIB)]);
        assert!(r.iter().all(|&(_, b)| b <= 32 * MIB));
    }
}
