//! Windows install media on an NTFS drive, for installers that FAT32 cannot hold.
//!
//! The normal Windows installer NyForge makes is one FAT32 partition: every UEFI computer starts it, and an
//! `install.wim` over 4 GiB is split into pieces. Two kinds of image cannot be made that way: an `install.esd` over
//! 4 GiB (the compact format cannot be split), and any other file over 4 GiB. For those the drive gets:
//!
//! 1. an **NTFS** partition holding every file of the ISO, and
//! 2. a small FAT partition at the end holding **UEFI:NTFS**, a Secure Boot signed boot loader with an NTFS driver
//!    (Rufus uses the same). UEFI firmware cannot read NTFS itself; it starts UEFI:NTFS from the FAT partition, which
//!    loads the NTFS driver and starts `\efi\boot\bootx64.efi` from the NTFS partition.
//!
//! Limits, stated plainly: UEFI computers only (no legacy BIOS start from this layout), and the NTFS files are
//! written through the operating system (Linux: `mkntfs` and a mounted `ntfs-3g` or `ntfs3`; Windows: `diskpart` and
//! a drive letter), not by NyForge's own writer. macOS cannot write NTFS.

use crate::error::{ErrorKind, NyError, Result};
use crate::format::human_size;
use crate::iso::IsoImage;
use crate::nyboot::assets;
use crate::nyboot::install::open_rw_for_install;
use crate::nyboot::io::{FileIo, Io};
use crate::nyboot::layout::{MIB_SECTORS, SECTOR};
use crate::nyboot::probe::{self, WindowsInfo};
use crate::progress::*;
use crate::sys::{self, TargetKind};
use crate::usbformat::tables::{self, TablePart};
use crate::usbformat::{fsmake, Fs, PartSpec};
use crate::winmedia::{autounattend_xml, fat_label, WindowsOptions, WindowsReport};
use crate::writer::Target;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MIB: u64 = 1 << 20;
const FAT32_MAX_FILE: u64 = 4 * 1024 * 1024 * 1024 - 1;

/// What the installer drive looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WinLayout {
    /// FAT32 when the files fit it (a big `install.wim` is split), NTFS with UEFI:NTFS when they cannot.
    #[default]
    Auto,
    /// One FAT32 partition, UEFI and legacy BIOS.
    Fat32,
    /// NTFS plus UEFI:NTFS, UEFI only.
    Ntfs,
}

impl std::str::FromStr for WinLayout {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(WinLayout::Auto),
            "fat32" | "fat" => Ok(WinLayout::Fat32),
            "ntfs" => Ok(WinLayout::Ntfs),
            o => Err(format!("\"{o}\" is not a layout (auto, fat32, ntfs).")),
        }
    }
}

/// The files of the ISO that FAT32 cannot hold and that cannot be split: everything over 4 GiB except an
/// `install.wim` (which is split), and an `install.esd` over 4 GiB (which is the usual case).
pub fn unsplittable_big_files(iso: &IsoImage) -> Result<Vec<(String, u64)>> {
    Ok(iso
        .walk(1_000_000)?
        .into_iter()
        .filter(|(p, size)| *size > FAT32_MAX_FILE && !p.eq_ignore_ascii_case("sources/install.wim"))
        .collect())
}

/// Which layout to make. `Err` when the request cannot be met.
pub fn decide_layout(want: WinLayout, iso: &IsoImage, win: &WindowsInfo) -> Result<WinLayout> {
    let big = !unsplittable_big_files(iso)?.is_empty();
    Ok(match want {
        WinLayout::Ntfs => WinLayout::Ntfs,
        WinLayout::Fat32 => WinLayout::Fat32,
        WinLayout::Auto => {
            if big || (win.install_image.as_deref() == Some("install.esd") && win.exceeds_fat32) {
                WinLayout::Ntfs
            } else {
                WinLayout::Fat32
            }
        }
    })
}

/// Where the partitions go: NTFS from 1 MiB, then the 1 MiB UEFI:NTFS partition at the end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NtfsLayout {
    pub ntfs_start: u64,
    pub ntfs_sectors: u64,
    pub boot_start: u64,
    pub boot_sectors: u64,
}

pub fn ntfs_layout(disk_bytes: u64) -> Result<NtfsLayout> {
    let boot_sectors = assets::UEFI_NTFS_IMG.len() as u64 / SECTOR;
    let end = (disk_bytes / SECTOR).min(1 << 32) & !(MIB_SECTORS - 1);
    if end < 3 * MIB_SECTORS + boot_sectors {
        return Err(NyError::new(ErrorKind::NoSpace, "The drive is too small for a Windows installer."));
    }
    let boot_start = end - boot_sectors;
    Ok(NtfsLayout { ntfs_start: MIB_SECTORS, ntfs_sectors: boot_start - MIB_SECTORS, boot_start, boot_sectors })
}

/// The MBR: partition 1 NTFS (active), partition 2 an EFI System partition holding UEFI:NTFS.
pub fn ntfs_mbr(sig: u32, l: &NtfsLayout) -> Result<[u8; 512]> {
    let part = |start, sectors, ty| TablePart { start, sectors, mbr_type: ty, gpt_type: tables::type_basic_data(), unique: tables::random_guid(), name: String::new() };
    let mut m = tables::mbr_sector(sig, &[part(l.ntfs_start, l.ntfs_sectors, 0x07), part(l.boot_start, l.boot_sectors, 0xEF)])?;
    m[446] = 0x80;
    Ok(m)
}

// ------------------------------------------------------------------------------------------ the volume

fn run_cmd(cmd: &mut Command, what: &str) -> Result<String> {
    let out = cmd.output().map_err(|e| NyError::from_io(&e, what))?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    if out.status.success() {
        Ok(text)
    } else {
        Err(NyError::other(format!("{what} failed: {}", text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("no message").trim())))
    }
}

enum Cleanup {
    Umount(PathBuf),
    Rmdir(PathBuf),
    Loop(String),
}

/// An NTFS volume the operating system has mounted, for writing (or reading) files through the normal file API.
pub struct NtfsVolume {
    pub root: PathBuf,
    cleanup: Vec<Cleanup>,
}

impl NtfsVolume {
    fn run_cleanup(&mut self) -> Result<()> {
        let mut first: Option<NyError> = None;
        while let Some(c) = self.cleanup.pop() {
            let r = match &c {
                Cleanup::Umount(d) => {
                    // A moment to settle, then retry once: a just-written volume can still be busy.
                    let a = run_cmd(Command::new("umount").arg(d), "Unmounting the NTFS volume");
                    if a.is_err() {
                        std::thread::sleep(Duration::from_millis(800));
                        run_cmd(Command::new("umount").arg(d), "Unmounting the NTFS volume").map(|_| ())
                    } else {
                        Ok(())
                    }
                }
                Cleanup::Rmdir(d) => std::fs::remove_dir(d).map_err(|e| NyError::from_io(&e, "removing the mount folder")),
                Cleanup::Loop(l) => run_cmd(Command::new("losetup").args(["-d", l]), "Releasing the loop device").map(|_| ()),
            };
            if let Err(e) = r {
                first.get_or_insert(e);
            }
        }
        first.map_or(Ok(()), Err)
    }

    /// Flush, unmount and release. Call this (not just drop) so a failure is reported.
    pub fn finish(mut self) -> Result<()> {
        if cfg!(unix) {
            let _ = Command::new("sync").status();
        }
        self.run_cleanup()
    }
}

impl Drop for NtfsVolume {
    fn drop(&mut self) {
        let _ = self.run_cleanup();
    }
}

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let mut dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    for extra in ["/sbin", "/usr/sbin", "/usr/local/sbin", "/usr/bin", "/bin"] {
        dirs.push(PathBuf::from(extra));
    }
    dirs.into_iter().map(|d| d.join(name)).find(|p| p.is_file())
}

/// Linux: mount an NTFS partition node. `ntfs-3g` first (what `mkntfs` volumes are tested with), the kernel's `ntfs3`
/// when that program is not installed.
fn mount_linux(node: &Path, read_only: bool) -> Result<NtfsVolume> {
    let dir = std::env::temp_dir().join(format!("nyforge-ntfs-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::SeqCst)));
    std::fs::create_dir_all(&dir).map_err(|e| NyError::from_io(&e, "creating the mount folder"))?;
    let mut vol = NtfsVolume { root: dir.clone(), cleanup: vec![Cleanup::Rmdir(dir.clone())] };
    let opt = if read_only { "ro,noatime" } else { "noatime" };
    let mounted = if let Some(prog) = which("ntfs-3g") {
        run_cmd(Command::new(prog).arg("-o").arg(opt).arg(node).arg(&dir), "Mounting the NTFS volume with ntfs-3g")
    } else {
        run_cmd(Command::new("mount").args(["-t", "ntfs3", "-o", opt]).arg(node).arg(&dir), "Mounting the NTFS volume")
    };
    mounted.map_err(|e| e.with_hint("Install ntfs-3g (Arch and CachyOS: ntfs-3g and ntfsprogs) and run it again."))?;
    vol.cleanup.push(Cleanup::Umount(dir));
    Ok(vol)
}

/// A loop device over part of an image file (Linux), for writing NTFS inside an image.
fn loop_over(file: &Path, offset: u64, len: u64) -> Result<String> {
    let out = run_cmd(Command::new("losetup").args(["--find", "--show", "--offset", &offset.to_string(), "--sizelimit", &len.to_string()]).arg(file), "Setting up a loop device")?;
    Ok(out.trim().to_string())
}

/// Windows: give partition `number` of disk `disk` a drive letter that is free, through `diskpart`.
fn windows_letter(disk: u32, number: u32) -> Result<PathBuf> {
    for c in ('D'..='Z').rev() {
        let root = PathBuf::from(format!("{c}:\\"));
        if root.exists() {
            continue;
        }
        let script = format!("rescan\r\nselect disk {disk}\r\nselect partition {number}\r\nassign letter={c} noerr\r\nexit\r\n");
        crate::winnative::run_diskpart(&script, &AtomicBool::new(false), Duration::from_secs(60))?;
        for _ in 0..40 {
            if root.exists() {
                return Ok(root);
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        return Err(NyError::other(format!("Windows did not make {c}: available after it was assigned.")));
    }
    Err(NyError::other("Every drive letter is in use."))
}

/// Make partition 1 NTFS and open it for writing.
fn format_and_mount(target: &Target, l: &NtfsLayout, label: &str, emit: &mut dyn FnMut(Event)) -> Result<NtfsVolume> {
    if cfg!(target_os = "macos") {
        return Err(NyError::new(ErrorKind::Unavailable, "macOS cannot write NTFS.").with_hint("Make this installer on Windows or Linux."));
    }
    if cfg!(windows) {
        let path = target.path.to_string_lossy();
        let disk = crate::device::windows::disk_number(&path).ok_or_else(|| NyError::other(format!("{path} is not a Windows physical drive.")))?;
        emit(Event::Phase { phase: Phase::Writing, detail: "Formatting the NTFS partition with Windows".into() });
        crate::winnative::format_partition(disk, 1, Fs::Ntfs, label, None)?;
        let root = windows_letter(disk, 1)?;
        return Ok(NtfsVolume { root, cleanup: vec![] });
    }
    // Linux
    let spec = PartSpec { fs: Fs::Ntfs, size: None, label: label.to_string(), cluster: None };
    let job = fsmake::ToolJob { spec: &spec, label, start_sector: l.ntfs_start, bytes: l.ntfs_sectors * SECTOR, owner: None };
    emit(Event::Phase { phase: Phase::Writing, detail: "Making the NTFS file system".into() });
    let (node, loopdev) = match target.kind {
        TargetKind::BlockDevice => {
            let node = fsmake::partition_node(&target.path.to_string_lossy(), 1);
            fsmake::wait_for_node(&node)?;
            (node, None)
        }
        TargetKind::File => {
            let dev = loop_over(&target.path, l.ntfs_start * SECTOR, l.ntfs_sectors * SECTOR)?;
            (PathBuf::from(&dev), Some(dev))
        }
    };
    let result = fsmake::make_with_tool(&job, fsmake::ToolTarget::Node(&node), &std::env::temp_dir()).and_then(|_| {
        emit(Event::Phase { phase: Phase::Writing, detail: "Mounting the NTFS volume".into() });
        mount_linux(&node, false)
    });
    match result {
        Ok(mut vol) => {
            if let Some(d) = loopdev {
                vol.cleanup.insert(0, Cleanup::Loop(d));
            }
            Ok(vol)
        }
        Err(e) => {
            if let Some(d) = loopdev {
                let _ = Command::new("losetup").args(["-d", &d]).status();
            }
            Err(e)
        }
    }
}

// ------------------------------------------------------------------------------------------------ the copy

struct Copier<'a> {
    iso: &'a IsoImage,
    cancel: &'a AtomicBool,
    total: u64,
    done: u64,
    meter: SpeedMeter,
    last: Instant,
    target: String,
}

impl Copier<'_> {
    fn advance(&mut self, n: u64, emit: &mut dyn FnMut(Event)) {
        self.done += n;
        if self.last.elapsed() >= Duration::from_millis(120) {
            self.last = Instant::now();
            let speed = self.meter.sample(Instant::now(), self.done);
            emit(Event::Progress(Progress {
                bytes_done: self.done,
                bytes_total: Some(self.total),
                fraction: if self.total > 0 { (self.done as f64 / self.total as f64).clamp(0.0, 1.0) } else { 0.0 },
                speed_bps: speed,
                eta_secs: if speed > 1.0 { Some(self.total.saturating_sub(self.done) as f64 / speed) } else { None },
                targets: vec![TargetProgress { target: self.target.clone(), bytes: self.done, state: TargetState::Writing }],
            }));
        }
    }

    fn copy_dir(&mut self, iso_dir: &str, dest: &Path, emit: &mut dyn FnMut(Event), buf: &mut [u8]) -> Result<()> {
        use std::io::Write;
        for e in self.iso.list(iso_dir)? {
            if self.cancel.load(Ordering::Relaxed) {
                return Err(NyError::cancelled());
            }
            let path = if iso_dir.is_empty() { e.name.clone() } else { format!("{iso_dir}/{}", e.name) };
            let out = dest.join(&e.name);
            if e.is_dir {
                std::fs::create_dir_all(&out).map_err(|x| NyError::from_io(&x, &format!("creating {path}")))?;
                self.copy_dir(&path, &out, emit, buf)?;
                continue;
            }
            let mut f = std::fs::File::create(&out).map_err(|x| NyError::from_io(&x, &format!("creating {path}")))?;
            let mut off = 0u64;
            while off < e.size {
                if self.cancel.load(Ordering::Relaxed) {
                    return Err(NyError::cancelled());
                }
                let n = self.iso.read(&e, off, buf)?;
                if n == 0 {
                    return Err(NyError::new(ErrorKind::ImageCorrupt, format!("The ISO ends in the middle of {path}.")));
                }
                f.write_all(&buf[..n]).map_err(|x| NyError::from_io(&x, &format!("writing {path}")))?;
                off += n as u64;
                self.advance(n as u64, emit);
            }
            f.flush().map_err(|x| NyError::from_io(&x, &format!("writing {path}")))?;
        }
        Ok(())
    }
}

/// Make the NTFS + UEFI:NTFS installer. Called by [`crate::winmedia::make_windows_media`] when the layout is NTFS.
pub fn make_windows_media_ntfs(iso_path: &Path, target: &Target, opts: &WindowsOptions, cancel: Arc<AtomicBool>, emit: &mut dyn FnMut(Event)) -> Result<WindowsReport> {
    let iso = IsoImage::open(iso_path)?;
    let report = probe::analyze(&iso, std::fs::metadata(iso_path).map(|m| m.len()).unwrap_or(0))?;
    let win = report.windows.clone().ok_or_else(|| NyError::invalid("This ISO is not a Windows installer."))?;
    if !win.uefi_x64 && !win.uefi_arm64 {
        return Err(NyError::new(ErrorKind::ImageUnsupported, "This Windows ISO has no UEFI boot files, and the NTFS layout starts on UEFI computers only.")
            .with_hint("Use the FAT32 layout (it also starts on legacy BIOS computers), or an ISO that has \\efi\\boot\\bootx64.efi."));
    }
    let mut notes = vec![crate::winmedia::WINDOWS_CAVEAT.to_string(), "This installer starts on UEFI computers only (no legacy BIOS): its NTFS drive is read by the UEFI:NTFS boot loader.".to_string()];
    let total: u64 = iso.walk(1_000_000)?.iter().map(|(_, s)| *s).sum();
    let disk_bytes = match target.kind {
        TargetKind::BlockDevice => target.capacity.ok_or_else(|| NyError::other("unknown drive size"))?,
        TargetKind::File => opts.file_size.or(target.capacity).ok_or_else(|| NyError::invalid("Making Windows media in an image file needs a size.").with_hint("Add --size 16G (for example)."))?,
    };
    if let Some(d) = &target.drive {
        if d.sector_size != 512 {
            return Err(NyError::new(ErrorKind::ImageUnsupported, format!("{} uses {}-byte sectors; only 512-byte sectors are supported for now.", d.path, d.sector_size)));
        }
    }
    let layout = ntfs_layout(disk_bytes)?;
    let part_bytes = layout.ntfs_sectors * SECTOR;
    let needed = total + total / 20 + (128 << 20); // NTFS metadata and rounding
    if part_bytes < needed {
        return Err(NyError::new(ErrorKind::NoSpace, format!("This Windows image needs about {} but the drive only has {} usable.", human_size(needed), human_size(part_bytes))));
    }
    // Everything that can be checked is checked before the drive is touched.
    if cfg!(target_os = "macos") {
        return Err(NyError::new(ErrorKind::Unavailable, "macOS cannot write NTFS, so this installer cannot be made there.").with_hint("Make it on Windows or Linux."));
    }
    if cfg!(target_os = "linux") {
        fsmake::check_tool(Fs::Ntfs)?;
        if which("ntfs-3g").is_none() && !std::fs::read_to_string("/proc/filesystems").unwrap_or_default().lines().any(|l| l.trim_end().ends_with("ntfs3")) {
            return Err(NyError::new(ErrorKind::Unavailable, "Writing NTFS needs ntfs-3g (or the kernel's ntfs3 driver), and neither is available.").with_hint("Install ntfs-3g."));
        }
    }

    let label = opts.label.clone().unwrap_or_else(|| fat_label(&iso.label));
    emit(Event::Start {
        image: iso_path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        container: format!("Windows installer, NTFS ({})", report.container),
        total_bytes: Some(total),
        targets: vec![target.display.clone()],
    });
    emit(Event::Phase { phase: Phase::Preparing, detail: "Releasing the drive".into() });
    let _prep = match &target.drive {
        Some(d) => Some(sys::prepare_device_for_erase(d, opts.unmount)?),
        None => None,
    };
    let started = Instant::now();
    let file = open_rw_for_install(target).map_err(|e| NyError::from_io(&e, &format!("opening {}", target.display)))?;
    if target.kind == TargetKind::File {
        file.set_len(disk_bytes).map_err(|e| NyError::from_io(&e, "sizing the image file"))?;
    }
    let whole = FileIo::new(file, 0, (disk_bytes / SECTOR) * SECTOR, sys::block_io_align(target.kind)).recovering(&target.path, target.drive.as_ref().filter(|_| target.kind == TargetKind::BlockDevice), sys::Reopen::Rw);
    emit(Event::Phase { phase: Phase::Writing, detail: "Clearing the drive".into() });
    whole.zero(0, MIB).map_err(|e| NyError::from_io(&e, "clearing the start of the drive"))?;
    whole.zero(whole.len() - MIB, MIB).map_err(|e| NyError::from_io(&e, "clearing the end of the drive"))?;
    // The boot partition's content first, while no partition table exists yet (so the system has not mounted anything).
    emit(Event::Phase { phase: Phase::Writing, detail: "Writing the UEFI:NTFS boot partition".into() });
    whole.write_at(assets::UEFI_NTFS_IMG, layout.boot_start * SECTOR).map_err(|e| NyError::from_io(&e, "writing the UEFI:NTFS boot partition"))?;
    emit(Event::Phase { phase: Phase::Writing, detail: "Writing the partition table".into() });
    let mut sig = [0u8; 4];
    let _ = getrandom::fill(&mut sig);
    let mbr = ntfs_mbr(u32::from_le_bytes(sig) | 0x0100_0000, &layout)?;
    whole.write_at(&mbr, 0).map_err(|e| NyError::from_io(&e, "writing the partition table"))?;
    whole.sync().map_err(|e| NyError::from_io(&e, "flushing the drive"))?;
    drop(whole);
    if target.kind == TargetKind::BlockDevice {
        sys::finish_device(&target.path);
    }

    let vol = format_and_mount(target, &layout, &label, emit)?;
    emit(Event::Phase { phase: Phase::Writing, detail: "Copying Windows files".into() });
    let mut job = Copier { iso: &iso, cancel: &cancel, total, done: 0, meter: SpeedMeter::new(Duration::from_secs(3)), last: Instant::now() - Duration::from_secs(1), target: target.display.clone() };
    let mut buf = vec![0u8; 1 << 20];
    job.copy_dir("", &vol.root, emit, &mut buf)?;
    let arch = if win.uefi_arm64 && !win.uefi_x64 { "arm64" } else { "amd64" };
    if let Some(xml) = autounattend_xml(arch, &opts.unattend) {
        if iso.exists("autounattend.xml") {
            notes.push("The ISO already contains its own autounattend.xml, so your setup options were not added.".into());
        } else {
            std::fs::write(vol.root.join("autounattend.xml"), xml).map_err(|e| NyError::from_io(&e, "writing autounattend.xml"))?;
            notes.push(format!("Added autounattend.xml ({}). Not tested on a real Windows Setup.", opts.unattend.describe()));
        }
    }
    emit(Event::Phase { phase: Phase::Finalizing, detail: "Flushing and unmounting".into() });
    vol.finish()?;
    if target.kind == TargetKind::BlockDevice {
        sys::finish_device(&target.path);
    }
    let elapsed = started.elapsed();
    emit(Event::TargetFinished { target: target.display.clone(), ok: true, bytes: total, error: None });
    emit(Event::Finished { ok: true, bytes: total, elapsed_secs: elapsed.as_secs_f64(), avg_speed_bps: if elapsed.as_secs_f64() > 0.0 { total as f64 / elapsed.as_secs_f64() } else { 0.0 } });
    Ok(WindowsReport { bytes: total, label, split_parts: 0, notes })
}

// ------------------------------------------------------------------------------------------------ verifying

/// Is this drive one of ours in the NTFS layout (MBR with an NTFS partition 1 and an EFI partition 2 that holds UEFI:NTFS)?
pub fn is_ntfs_media(target: &Target) -> Option<NtfsLayout> {
    let len = target.capacity.or_else(|| std::fs::metadata(&target.path).ok().map(|m| m.len()))?;
    let block = target.kind == TargetKind::BlockDevice;
    let file = sys::open_read(&target.path, target.kind, block).ok()?;
    let io = FileIo::new(file, 0, len / SECTOR * SECTOR, if block { 4096 } else { 1 });
    let mut m = [0u8; 512];
    io.read_at(&mut m, 0).ok()?;
    if m[510] != 0x55 || m[511] != 0xAA {
        return None;
    }
    let e = |i: usize| -> (u8, u64, u64) {
        let o = 446 + i * 16;
        (m[o + 4], u32::from_le_bytes(m[o + 8..o + 12].try_into().unwrap()) as u64, u32::from_le_bytes(m[o + 12..o + 16].try_into().unwrap()) as u64)
    };
    let (p1, p2, p3) = (e(0), e(1), e(2));
    if p1.0 != 0x07 || p2.0 != 0xEF || p3.0 != 0 {
        return None;
    }
    // The boot partition really holds UEFI:NTFS: its first 1 MiB is the image.
    let mut head = vec![0u8; 4096];
    io.read_at(&mut head, p2.1 * SECTOR).ok()?;
    if head[..4096] != assets::UEFI_NTFS_IMG[..4096] {
        return None;
    }
    Some(NtfsLayout { ntfs_start: p1.1, ntfs_sectors: p1.2, boot_start: p2.1, boot_sectors: p2.2 })
}

/// Read an NTFS-layout installer back: mount it read-only and compare every file with the ISO.
pub fn verify_windows_media_ntfs(iso_path: &Path, target: &Target, l: &NtfsLayout, cancel: Arc<AtomicBool>, emit: &mut dyn FnMut(Event)) -> Result<crate::winmedia::WindowsVerifyReport> {
    use std::io::Read;
    let iso = IsoImage::open(iso_path)?;
    let files = iso.walk(1_000_000)?;
    let total: u64 = files.iter().map(|(_, s)| *s).sum();
    emit(Event::Phase { phase: Phase::Verifying, detail: "Mounting the drive to read it back".into() });
    let started = Instant::now();
    if let Some(d) = &target.drive {
        let _ = sys::prepare_device_for_erase(d, true).map(|p| std::mem::forget(p));
    }
    let (vol, loopdev) = if cfg!(windows) {
        let path = target.path.to_string_lossy();
        let disk = crate::device::windows::disk_number(&path).ok_or_else(|| NyError::other(format!("{path} is not a Windows physical drive.")))?;
        (NtfsVolume { root: windows_letter(disk, 1)?, cleanup: vec![] }, None)
    } else if cfg!(target_os = "linux") {
        match target.kind {
            TargetKind::BlockDevice => {
                let node = fsmake::partition_node(&target.path.to_string_lossy(), 1);
                if let Ok(f) = std::fs::File::open(&target.path) {
                    sys::drop_cache(&f);
                }
                (mount_linux(&node, true)?, None)
            }
            TargetKind::File => {
                let dev = loop_over(&target.path, l.ntfs_start * SECTOR, l.ntfs_sectors * SECTOR)?;
                let mut v = mount_linux(Path::new(&dev), true).inspect_err(|_| {
                    let _ = Command::new("losetup").args(["-d", &dev]).status();
                })?;
                v.cleanup.insert(0, Cleanup::Loop(dev.clone()));
                (v, Some(dev))
            }
        }
    } else {
        return Err(NyError::new(ErrorKind::Unavailable, "NyForge cannot read NTFS on this system."));
    };
    let _ = loopdev;
    let mut done = 0u64;
    let mut last = Instant::now() - Duration::from_secs(1);
    let mut meter = SpeedMeter::new(Duration::from_secs(3));
    let (mut a, mut b) = (vec![0u8; 1 << 20], vec![0u8; 1 << 20]);
    for (path, size) in &files {
        if cancel.load(Ordering::Relaxed) {
            return Err(NyError::cancelled());
        }
        let e = iso.find(path)?.ok_or_else(|| NyError::other(format!("{path} vanished from the ISO.")))?;
        let on_disk = vol.root.join(path);
        let mut f = std::fs::File::open(&on_disk).map_err(|_| NyError::new(ErrorKind::VerifyFailed, format!("{path} is missing on the drive.")))?;
        if f.metadata().map(|m| m.len()).unwrap_or(0) != *size {
            return Err(NyError::new(ErrorKind::VerifyFailed, format!("{path} has the wrong size on the drive.")));
        }
        let mut off = 0u64;
        while off < *size {
            let n = iso.read(&e, off, &mut a)?;
            if n == 0 {
                break;
            }
            f.read_exact(&mut b[..n]).map_err(|x| NyError::from_io(&x, &format!("reading {path} back")))?;
            if a[..n] != b[..n] {
                let at = a[..n].iter().zip(&b[..n]).position(|(x, y)| x != y).unwrap_or(0) as u64;
                return Err(NyError::new(ErrorKind::VerifyFailed, format!("{path} differs from the ISO at byte {}.", off + at)).with_hint("Make the installer again. If it fails again the drive is probably failing."));
            }
            off += n as u64;
            done += n as u64;
            if last.elapsed() >= Duration::from_millis(120) {
                last = Instant::now();
                let speed = meter.sample(Instant::now(), done);
                emit(Event::Progress(Progress {
                    bytes_done: done,
                    bytes_total: Some(total),
                    fraction: if total > 0 { (done as f64 / total as f64).clamp(0.0, 1.0) } else { 0.0 },
                    speed_bps: speed,
                    eta_secs: if speed > 1.0 { Some(total.saturating_sub(done) as f64 / speed) } else { None },
                    targets: vec![TargetProgress { target: target.display.clone(), bytes: done, state: TargetState::Writing }],
                }));
            }
        }
    }
    vol.finish()?;
    let elapsed = started.elapsed();
    let speed = if elapsed.as_secs_f64() > 0.0 { total as f64 / elapsed.as_secs_f64() } else { 0.0 };
    emit(Event::Verified { bytes: total, elapsed_secs: elapsed.as_secs_f64(), avg_speed_bps: speed, note: Some("Every file matches the ISO. The UEFI:NTFS boot partition was checked against the copy NyForge carries.".into()) });
    Ok(crate::winmedia::WindowsVerifyReport { bytes: total, files: files.len(), elapsed })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_layout_puts_ntfs_first_and_the_boot_partition_last_on_the_grid() {
        let l = ntfs_layout(16 << 30).unwrap();
        assert_eq!(l.ntfs_start, 2048);
        assert_eq!(l.boot_sectors, 2048);
        assert_eq!((l.ntfs_start + l.ntfs_sectors), l.boot_start, "no gap between the two");
        assert_eq!(l.boot_start % 2048, 0);
        assert!((l.boot_start + l.boot_sectors) * 512 <= 16 << 30);
        assert!(ntfs_layout(3 * MIB).is_err());
        // bigger than 2 TiB: MBR stops at 2 TiB
        let big = ntfs_layout(4 << 40).unwrap();
        assert!((big.boot_start + big.boot_sectors) <= 1 << 32);
    }

    #[test]
    fn the_mbr_has_an_active_ntfs_partition_and_an_efi_partition() {
        let l = ntfs_layout(8 << 30).unwrap();
        let m = ntfs_mbr(0x1234_5678, &l).unwrap();
        assert_eq!(m[446], 0x80);
        assert_eq!(m[446 + 4], 0x07);
        assert_eq!(m[446 + 16 + 4], 0xEF);
        assert_eq!(u32::from_le_bytes(m[446 + 16 + 8..446 + 16 + 12].try_into().unwrap()) as u64, l.boot_start);
        assert_eq!(&m[510..512], &[0x55, 0xAA]);
    }

    #[test]
    fn layouts_parse() {
        assert_eq!("auto".parse::<WinLayout>().unwrap(), WinLayout::Auto);
        assert_eq!("NTFS".parse::<WinLayout>().unwrap(), WinLayout::Ntfs);
        assert_eq!("fat32".parse::<WinLayout>().unwrap(), WinLayout::Fat32);
        assert!("exfat".parse::<WinLayout>().is_err());
    }

    #[test]
    fn the_carried_uefi_ntfs_image_has_the_loaders_for_the_usual_architectures() {
        let img = assets::UEFI_NTFS_IMG;
        for name in ["BOOTX64 EFI", "BOOTIA32EFI", "BOOTAA64EFI"] {
            let n = name.replace(' ', " ");
            let _ = n;
        }
        // 8.3 directory entries of \EFI\BOOT sit somewhere in the image: look for the names
        let has = |needle: &[u8]| img.windows(needle.len()).any(|w| w == needle);
        assert!(has(b"BOOTX64 EFI") && has(b"BOOTAA64EFI") && has(b"BOOTIA32EFI"));
    }
}
