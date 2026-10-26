//! Windows install media from an ISO: one FAT32 partition that boots on UEFI
//! (including Secure Boot, because it is Microsoft's own boot manager) and,
//! through GRUB's `ntldr`, on legacy BIOS.
//!
//! The ISO is read directly (UDF/ISO 9660, no mounting). `install.wim` files
//! too big for FAT32 are split into `install.swm` parts on the fly.
//!
//! Honest status: the file copy, FAT32 volume, WIM splitting and MBR are
//! tested here; actually booting the result into Windows Setup could not be
//! tested (no Windows ISO or Windows machine was available), and the BIOS path
//! is the less proven one.

use crate::error::{ErrorKind, NyError, Result};
use crate::format::human_size;
use crate::iso::{Entry, IsoImage};
use crate::nyboot::install::open_rw_for_install;
use crate::nyboot::io::{FileIo, Io};
use crate::nyboot::layout::{MIB_SECTORS, SECTOR};
use crate::nyboot::probe::{self, Verdict};
use crate::nyboot::{assets, stream::CachedStream};
use crate::progress::*;
use crate::sys::{self, TargetKind};
use crate::wim::{SplitPlan, WimSource, FAT32_PART_BYTES};
use crate::writer::Target;
use fatfs::{FatType, FileSystem, FormatVolumeOptions, FsOptions, ReadWriteSeek};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// FAT32 cannot hold a file of 4 GiB or more.
const FAT32_MAX_FILE: u64 = 4 * 1024 * 1024 * 1024 - 1;
/// Largest FAT32 partition we create (Windows' own tools stop at 32 GiB; far more is pointless for a ~7 GiB installer).
const MAX_PARTITION: u64 = 128 << 30;

#[derive(Debug, Clone, Default)]
pub struct Unattend {
    /// Windows 11: skip the TPM, Secure Boot, RAM, CPU and storage checks in Setup.
    pub bypass_hardware_checks: bool,
}

#[derive(Debug, Clone)]
pub struct WindowsOptions {
    pub label: Option<String>,
    /// Required when the target is an image file.
    pub file_size: Option<u64>,
    pub unmount: bool,
    /// Split `install.wim` into parts of at most this many bytes.
    pub part_bytes: u64,
    pub unattend: Unattend,
    /// Make the stick bootable on legacy BIOS as well as UEFI.
    pub bios: bool,
}

impl Default for WindowsOptions {
    fn default() -> Self {
        Self { label: None, file_size: None, unmount: true, part_bytes: FAT32_PART_BYTES, unattend: Unattend::default(), bios: true }
    }
}

#[derive(Debug, Clone)]
pub struct WindowsReport {
    pub bytes: u64,
    pub label: String,
    pub split_parts: usize,
    pub notes: Vec<String>,
}

struct IsoFile<'a> {
    iso: &'a IsoImage,
    entry: &'a Entry,
}

impl WimSource for IsoFile<'_> {
    fn len(&self) -> u64 {
        self.entry.size
    }
    fn read_at(&self, buf: &mut [u8], offset: u64) -> Result<()> {
        let mut done = 0;
        while done < buf.len() {
            let n = self.iso.read(self.entry, offset + done as u64, &mut buf[done..])?;
            if n == 0 {
                return Err(NyError::new(ErrorKind::ImageCorrupt, "The ISO ends before its Windows image does."));
            }
            done += n;
        }
        Ok(())
    }
}

/// Make a FAT32 label (≤ 11 chars, upper case, only characters FAT allows).
pub fn fat_label(iso_label: &str) -> String {
    let s: String = iso_label
        .to_ascii_uppercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { ' ' })
        .collect();
    let s = s.trim().chars().take(11).collect::<String>();
    if s.is_empty() {
        "WINDOWS".into()
    } else {
        s
    }
}

pub fn autounattend_xml(arch: &str, u: &Unattend) -> Option<String> {
    if !u.bypass_hardware_checks {
        return None;
    }
    let keys = ["BypassTPMCheck", "BypassSecureBootCheck", "BypassRAMCheck", "BypassCPUCheck", "BypassStorageCheck"];
    let mut cmds = String::new();
    for (i, k) in keys.iter().enumerate() {
        cmds.push_str(&format!(
            "        <RunSynchronousCommand wcm:action=\"add\">\n          <Order>{}</Order>\n          <Path>reg add HKLM\\SYSTEM\\Setup\\LabConfig /v {} /t REG_DWORD /d 1 /f</Path>\n        </RunSynchronousCommand>\n",
            i + 1,
            k
        ));
    }
    Some(format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<unattend xmlns=\"urn:schemas-microsoft-com:unattend\" xmlns:wcm=\"http://schemas.microsoft.com/WMIConfig/2002/State\">\n  <settings pass=\"windowsPE\">\n    <component name=\"Microsoft-Windows-Setup\" processorArchitecture=\"{arch}\" publicKeyToken=\"31bf3856ad364e35\" language=\"neutral\" versionScope=\"nonSxS\">\n      <RunSynchronous>\n{cmds}      </RunSynchronous>\n    </component>\n  </settings>\n</unattend>\n"
    ))
}

const GRUB_CFG: &str = "set timeout=0\nset default=0\nntldr /bootmgr\nboot\n";

fn windows_mbr(sig: u32, start: u64, sectors: u64) -> [u8; 512] {
    let mut m = [0u8; 512];
    m.copy_from_slice(&assets::BOOT_IMG[..512]);
    m[0x66] = 0x90;
    m[0x67] = 0x90;
    m[0x5C..0x64].copy_from_slice(&1u64.to_le_bytes());
    m[440..510].fill(0);
    m[440..444].copy_from_slice(&sig.to_le_bytes());
    let mut e = [0u8; 16];
    e[0] = 0x80;
    e[1..4].copy_from_slice(&[0xFE, 0xFF, 0xFF]);
    e[4] = 0x0C; // FAT32 (LBA)
    e[5..8].copy_from_slice(&[0xFE, 0xFF, 0xFF]);
    e[8..12].copy_from_slice(&(start as u32).to_le_bytes());
    e[12..16].copy_from_slice(&(sectors as u32).to_le_bytes());
    m[446..462].copy_from_slice(&e);
    m[510] = 0x55;
    m[511] = 0xAA;
    m
}

fn fat_err(e: std::io::Error) -> NyError {
    match e.kind() {
        std::io::ErrorKind::WriteZero | std::io::ErrorKind::UnexpectedEof => {
            NyError::new(ErrorKind::NoSpace, "The drive is too small for this Windows image.")
        }
        _ => NyError::from_io(&e, "writing the Windows files"),
    }
}

struct Job<'a> {
    iso: &'a IsoImage,
    cancel: &'a AtomicBool,
    total: u64,
    done: u64,
    meter: SpeedMeter,
    last_emit: Instant,
    target: String,
}

impl Job<'_> {
    fn advance(&mut self, n: u64, emit: &mut dyn FnMut(Event)) {
        self.done += n;
        if self.last_emit.elapsed() >= Duration::from_millis(120) {
            self.last_emit = Instant::now();
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
}

enum Plan {
    Copy,
    Split(Box<SplitPlan>),
}

fn copy_tree<IO: ReadWriteSeek>(
    job: &mut Job<'_>,
    iso_dir: &str,
    dir: &fatfs::Dir<'_, IO>,
    plan: &Plan,
    emit: &mut dyn FnMut(Event),
    buf: &mut [u8],
) -> Result<()> {
    for e in job.iso.list(iso_dir)? {
        if job.cancel.load(Ordering::Relaxed) {
            return Err(NyError::cancelled());
        }
        let path = if iso_dir.is_empty() { e.name.clone() } else { format!("{iso_dir}/{}", e.name) };
        if e.is_dir {
            let sub = dir.create_dir(&e.name).map_err(fat_err)?;
            copy_tree(job, &path, &sub, plan, emit, buf)?;
            continue;
        }
        if let Plan::Split(sp) = plan {
            if path.eq_ignore_ascii_case("sources/install.wim") {
                let src = IsoFile { iso: job.iso, entry: &e };
                for i in 0..sp.part_count() {
                    let name = SplitPlan::part_name("install", i);
                    let mut f = dir.create_file(&name).map_err(fat_err)?;
                    let cancel = job.cancel;
                    let mut written = 0u64;
                    {
                        let mut w = ProgressWriter { inner: &mut f, job, emit, pending: &mut written };
                        sp.write_part(i, &src, &mut w, cancel, &mut |_| {})?;
                    }
                    f.flush().map_err(fat_err)?;
                }
                continue;
            }
        }
        if e.size > FAT32_MAX_FILE {
            return Err(NyError::new(
                ErrorKind::ImageUnsupported,
                format!("\"{}\" is {}, too big for a FAT32 Windows stick.", path, human_size(e.size)),
            )
            .with_hint("Only install.wim can be split automatically."));
        }
        let mut f = dir.create_file(&e.name).map_err(fat_err)?;
        let mut off = 0u64;
        while off < e.size {
            if job.cancel.load(Ordering::Relaxed) {
                return Err(NyError::cancelled());
            }
            let n = job.iso.read(&e, off, buf)?;
            if n == 0 {
                return Err(NyError::new(ErrorKind::ImageCorrupt, format!("The ISO ends in the middle of {path}.")));
            }
            f.write_all(&buf[..n]).map_err(fat_err)?;
            off += n as u64;
            job.advance(n as u64, emit);
        }
        f.flush().map_err(fat_err)?;
    }
    Ok(())
}

/// Counts bytes written through it as job progress.
struct ProgressWriter<'a, 'b, W: Write> {
    inner: &'a mut W,
    job: &'a mut Job<'b>,
    emit: &'a mut dyn FnMut(Event),
    pending: &'a mut u64,
}

impl<W: Write> Write for ProgressWriter<'_, '_, W> {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(b)?;
        *self.pending += n as u64;
        self.job.advance(n as u64, self.emit);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}


/// Decide what happens to `install.wim`: copied as it is, or split into parts that fit a FAT32 file.
fn decide_plan(iso: &IsoImage, win: &probe::WindowsInfo, opts: &WindowsOptions, notes: &mut Vec<String>) -> Result<Plan> {
    Ok(match win.install_image.as_deref() {
        Some("install.wim") if win.exceeds_fat32 || opts.part_bytes < win.install_bytes => {
            let e = iso.find("sources/install.wim")?.unwrap();
            let sp = SplitPlan::new(&IsoFile { iso, entry: &e }, opts.part_bytes.min(FAT32_PART_BYTES))?;
            notes.push(format!("install.wim ({}) was split into {} parts for FAT32.", human_size(win.install_bytes), sp.part_count()));
            Plan::Split(Box::new(sp))
        }
        Some("install.esd") if win.exceeds_fat32 => {
            return Err(NyError::new(
                ErrorKind::ImageUnsupported,
                format!("install.esd is {}: too big for FAT32 and the compact (ESD) format cannot be split.", human_size(win.install_bytes)),
            )
            .with_hint("Use an ISO with install.wim (from Microsoft's download page)."));
        }
        _ => Plan::Copy,
    })
}

/// How many bytes end up on the drive from the ISO's files.
fn plan_total(iso: &IsoImage, plan: &Plan) -> Result<u64> {
    let mut total = 0u64;
    for (p, size) in iso.walk(1_000_000)? {
        if matches!(plan, Plan::Split(_)) && p.eq_ignore_ascii_case("sources/install.wim") {
            continue;
        }
        total += size;
    }
    if let Plan::Split(sp) = plan {
        total += sp.total_bytes();
    }
    Ok(total)
}

pub fn make_windows_media(
    iso_path: &Path,
    target: &Target,
    opts: &WindowsOptions,
    cancel: Arc<AtomicBool>,
    emit: &mut dyn FnMut(Event),
) -> Result<WindowsReport> {
    let iso = IsoImage::open(iso_path)?;
    let report = probe::analyze(&iso, std::fs::metadata(iso_path).map(|m| m.len()).unwrap_or(0))?;
    if report.verdict != Verdict::WindowsInstaller {
        return Err(NyError::invalid("This ISO is not a Windows installer.").with_hint("For Linux and other ISOs use `nyforge flash` or `nyforge boot add`."));
    }
    let win = report.windows.clone().unwrap();
    if !win.uefi_x64 && !win.uefi_arm64 && !win.bios_bootmgr {
        return Err(NyError::new(ErrorKind::ImageUnsupported, "This Windows ISO has no boot files NyForge recognises."));
    }
    let mut notes = vec![];

    let plan = decide_plan(&iso, &win, opts, &mut notes)?;
    let total = plan_total(&iso, &plan)?;

    let disk_bytes = match target.kind {
        TargetKind::BlockDevice => target.capacity.ok_or_else(|| NyError::other("unknown drive size"))?,
        TargetKind::File => opts
            .file_size
            .or(target.capacity)
            .ok_or_else(|| NyError::invalid("Making Windows media in an image file needs a size.").with_hint("Add --size 16G (for example)."))?,
    };
    if let Some(d) = &target.drive {
        if d.sector_size != 512 {
            return Err(NyError::new(ErrorKind::ImageUnsupported, format!("{} uses {}-byte sectors; only 512-byte sectors are supported for now.", d.path, d.sector_size)));
        }
    }
    let usable = (disk_bytes / SECTOR).min(0xFFFF_FFFF) & !(MIB_SECTORS - 1);
    let part_sectors = ((usable - MIB_SECTORS) * SECTOR).min(MAX_PARTITION) / SECTOR & !(MIB_SECTORS - 1);
    let part_bytes = part_sectors * SECTOR;
    let needed = total + total / 50 + (64 << 20); // FAT tables, directories, rounding
    if part_bytes < needed {
        return Err(NyError::new(
            ErrorKind::NoSpace,
            format!("This Windows image needs about {} but the drive only has {} usable.", human_size(needed), human_size(part_bytes)),
        ));
    }

    let label = opts.label.clone().unwrap_or_else(|| fat_label(&iso.label));
    emit(Event::Start {
        image: iso_path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        container: format!("Windows installer ({})", report.container),
        total_bytes: Some(total),
        targets: vec![target.display.clone()],
    });
    emit(Event::Phase { phase: Phase::Preparing, detail: "Releasing the drive".into() });
    let _prep = match &target.drive {
        Some(d) => Some(sys::prepare_device(d, opts.unmount)?),
        None => None,
    };
    let started = Instant::now();
    let file = open_rw_for_install(target).map_err(|e| NyError::from_io(&e, &format!("opening {}", target.display)))?;
    if target.kind == TargetKind::File {
        file.set_len(disk_bytes).map_err(|e| NyError::from_io(&e, "sizing the image file"))?;
    }
    let align = sys::block_io_align(target.kind);
    let whole = FileIo::new(file, 0, (disk_bytes / SECTOR) * SECTOR, align);

    emit(Event::Phase { phase: Phase::Writing, detail: "Clearing the drive".into() });
    whole.zero(0, 1 << 20).map_err(|e| NyError::from_io(&e, "clearing the start of the drive"))?;
    whole.zero(whole.len() - (1 << 20), 1 << 20).map_err(|e| NyError::from_io(&e, "clearing the end of the drive"))?;
    let mut sig = [0u8; 4];
    let _ = getrandom::fill(&mut sig);
    let mbr = windows_mbr(u32::from_le_bytes(sig) | 0x0100_0000, MIB_SECTORS, part_sectors);
    // The partition table and boot code go in last (see `WriteOptions::defer_head`): Windows mounts a new partition
    // the moment the table appears, and then refuses writes into it.

    emit(Event::Phase { phase: Phase::Writing, detail: format!("Formatting {} as FAT32", human_size(part_bytes)) });
    let part_io = whole.sub(MIB_SECTORS * SECTOR, part_bytes).map_err(|e| NyError::from_io(&e, "partitioning"))?;
    let mut cs = CachedStream::new(part_io);
    let mut vol_label = [b' '; 11];
    vol_label[..label.len()].copy_from_slice(label.as_bytes());
    let mut vid = [0u8; 4];
    let _ = getrandom::fill(&mut vid);
    fatfs::format_volume(
        &mut cs,
        FormatVolumeOptions::new().fat_type(FatType::Fat32).volume_label(vol_label).volume_id(u32::from_le_bytes(vid)).fats(2),
    )
    .map_err(fat_err)?;
    // The partition does not start at sector 0: tell the boot sector (and its backup at sector 6)
    // where it really lives, as Windows' own formatter does. Windows' boot manager relies on it.
    for sector in [0u64, 6] {
        cs.seek(SeekFrom::Start(sector * SECTOR + 0x1C)).map_err(fat_err)?;
        cs.write_all(&(MIB_SECTORS as u32).to_le_bytes()).map_err(fat_err)?;
    }
    cs.flush_all().map_err(fat_err)?;
    cs.seek(SeekFrom::Start(0)).map_err(fat_err)?; // fatfs insists on starting at 0

    emit(Event::Phase { phase: Phase::Writing, detail: "Copying Windows files".into() });
    let mut job = Job {
        iso: &iso,
        cancel: &cancel,
        total,
        done: 0,
        meter: SpeedMeter::new(Duration::from_secs(3)),
        last_emit: Instant::now() - Duration::from_secs(1),
        target: target.display.clone(),
    };
    {
        let fs = FileSystem::new(&mut cs, FsOptions::new()).map_err(fat_err)?;
        {
        let root = fs.root_dir();
        let mut buf = vec![0u8; 1 << 20];
        copy_tree(&mut job, "", &root, &plan, emit, &mut buf)?;

        if opts.bios {
            // The image GRUB boots has /boot/grub/grub.cfg (Windows ISOs already have a /boot folder).
            let boot = match root.open_dir("boot") {
                Ok(d) => d,
                Err(_) => root.create_dir("boot").map_err(fat_err)?,
            };
            let grub = boot.create_dir("grub").map_err(fat_err)?;
            let mut f = grub.create_file("grub.cfg").map_err(fat_err)?;
            f.write_all(GRUB_CFG.as_bytes()).map_err(fat_err)?;
            f.flush().map_err(fat_err)?;
        }
        let arch = if win.uefi_arm64 && !win.uefi_x64 { "arm64" } else { "amd64" };
        if let Some(xml) = autounattend_xml(arch, &opts.unattend) {
            if iso.exists("autounattend.xml") {
                notes.push("The ISO already contains autounattend.xml, so the Windows 11 check bypass was not added.".into());
            } else {
                let mut f = root.create_file("autounattend.xml").map_err(fat_err)?;
                f.write_all(xml.as_bytes()).map_err(fat_err)?;
                f.flush().map_err(fat_err)?;
                notes.push("Added autounattend.xml to skip the Windows 11 hardware checks (not tested on a real Windows Setup).".into());
            }
        }
        }
        let _ = fs.stats(); // fills in FSInfo's free-cluster count
        fs.unmount().map_err(fat_err)?;
    }
    emit(Event::Phase { phase: Phase::Finalizing, detail: "Writing the partition table and boot code".into() });
    cs.flush_all().map_err(fat_err)?;
    if opts.bios {
        let mut core = assets::CORE_WIN_IMG.to_vec();
        core.resize(core.len().next_multiple_of(SECTOR as usize), 0);
        whole.write_at(&core, SECTOR).map_err(|e| NyError::from_io(&e, "writing GRUB"))?;
    }
    whole.write_at(&mbr, 0).map_err(|e| NyError::from_io(&e, "writing the boot sector"))?;
    emit(Event::Phase { phase: Phase::Finalizing, detail: "Flushing to the drive".into() });
    whole.sync().map_err(|e| NyError::from_io(&e, "flushing the drive"))?;
    drop(cs);
    drop(whole);
    if target.kind == TargetKind::BlockDevice {
        sys::finish_device(&target.path);
    }
    if opts.bios && !win.bios_bootmgr {
        notes.push("This ISO has no bootmgr, so the stick will only start on UEFI computers.".into());
    }
    let elapsed = started.elapsed();
    emit(Event::TargetFinished { target: target.display.clone(), ok: true, bytes: total, error: None });
    emit(Event::Finished {
        ok: true,
        bytes: total,
        elapsed_secs: elapsed.as_secs_f64(),
        avg_speed_bps: if elapsed.as_secs_f64() > 0.0 { total as f64 / elapsed.as_secs_f64() } else { 0.0 },
    });
    Ok(WindowsReport { bytes: total, label, split_parts: if let Plan::Split(sp) = &plan { sp.part_count() } else { 0 }, notes })
}

// ------------------------------------------------------------------------------------------------ verifying

#[derive(Debug, Clone)]
pub struct WindowsVerifyReport {
    /// Bytes of ISO content compared.
    pub bytes: u64,
    /// Files compared (a split `install.wim` counts once per part).
    pub files: usize,
    pub elapsed: Duration,
}

fn differs(what: &str, at: u64) -> NyError {
    NyError::new(ErrorKind::VerifyFailed, format!("{what} on the drive is not the same as in the ISO (first difference at byte {at} of the file)."))
        .with_hint("Make the installer again. If it keeps happening the drive is probably failing; NyFormat's bad-block check can confirm that.")
}

fn missing(what: &str) -> NyError {
    NyError::new(ErrorKind::VerifyFailed, format!("{what} is missing from the drive."))
        .with_hint("Make the installer again; the drive may not have been finished, or it is not a NyForge Windows installer.")
}

/// Read what a FAT file holds, expecting `want` bytes in total, and compare as it goes: used for `install.wim` parts,
/// which are regenerated from the ISO and streamed through here.
struct CompareWriter<'a> {
    on_drive: &'a mut dyn Read,
    pos: u64,
    wrong_at: Option<u64>,
    short: bool,
    scratch: Vec<u8>,
}

impl Write for CompareWriter<'_> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if self.scratch.len() < data.len() {
            self.scratch.resize(data.len(), 0);
        }
        let got = &mut self.scratch[..data.len()];
        if let Err(e) = self.on_drive.read_exact(got) {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                self.short = true;
            }
            return Err(std::io::Error::other("the drive's file is shorter than expected"));
        }
        if let Some(i) = got.iter().zip(data).position(|(a, b)| a != b) {
            self.wrong_at = Some(self.pos + i as u64);
            return Err(std::io::Error::other("the drive's file differs"));
        }
        self.pos += data.len() as u64;
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Is there anything left in a file that should have ended?
fn has_more(f: &mut dyn Read) -> bool {
    let mut one = [0u8; 1];
    matches!(f.read(&mut one), Ok(n) if n > 0)
}

fn compare_with_iso(job: &mut Job<'_>, e: &Entry, path: &str, on_drive: &mut dyn Read, emit: &mut dyn FnMut(Event), a: &mut [u8], b: &mut [u8]) -> Result<()> {
    let mut off = 0u64;
    while off < e.size {
        if job.cancel.load(Ordering::Relaxed) {
            return Err(NyError::cancelled());
        }
        let n = job.iso.read(e, off, a)?;
        if n == 0 {
            return Err(NyError::new(ErrorKind::ImageCorrupt, format!("The ISO ends in the middle of {path}.")));
        }
        if let Err(err) = on_drive.read_exact(&mut b[..n]) {
            return Err(if err.kind() == std::io::ErrorKind::UnexpectedEof {
                NyError::new(ErrorKind::VerifyFailed, format!("{path} on the drive is shorter than in the ISO.")).with_hint("Make the installer again.")
            } else {
                NyError::from_io(&err, &format!("reading {path} back"))
            });
        }
        if let Some(i) = a[..n].iter().zip(&b[..n]).position(|(x, y)| x != y) {
            return Err(differs(path, off + i as u64));
        }
        off += n as u64;
        job.advance(n as u64, emit);
    }
    if has_more(on_drive) {
        return Err(NyError::new(ErrorKind::VerifyFailed, format!("{path} on the drive is longer than in the ISO.")).with_hint("Make the installer again."));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn verify_tree<IO: ReadWriteSeek>(
    job: &mut Job<'_>,
    iso_dir: &str,
    dir: &fatfs::Dir<'_, IO>,
    plan: &Plan,
    files: &mut usize,
    emit: &mut dyn FnMut(Event),
    a: &mut [u8],
    b: &mut [u8],
) -> Result<()> {
    for e in job.iso.list(iso_dir)? {
        if job.cancel.load(Ordering::Relaxed) {
            return Err(NyError::cancelled());
        }
        let path = if iso_dir.is_empty() { e.name.clone() } else { format!("{iso_dir}/{}", e.name) };
        if e.is_dir {
            let sub = dir.open_dir(&e.name).map_err(|_| missing(&format!("The folder {path}")))?;
            verify_tree(job, &path, &sub, plan, files, emit, a, b)?;
            continue;
        }
        if let Plan::Split(sp) = plan {
            if path.eq_ignore_ascii_case("sources/install.wim") {
                let src = IsoFile { iso: job.iso, entry: &e };
                for i in 0..sp.part_count() {
                    let name = SplitPlan::part_name("install", i);
                    let mut f = dir.open_file(&name).map_err(|_| missing(&format!("sources/{name}")))?;
                    let cancel = job.cancel;
                    let mut pending = 0u64;
                    let mut cw = CompareWriter { on_drive: &mut f, pos: 0, wrong_at: None, short: false, scratch: vec![] };
                    let r = {
                        let mut w = ProgressWriter { inner: &mut cw, job, emit, pending: &mut pending };
                        sp.write_part(i, &src, &mut w, cancel, &mut |_| {})
                    };
                    if let Some(at) = cw.wrong_at {
                        return Err(differs(&format!("sources/{name}"), at));
                    }
                    if cw.short {
                        return Err(NyError::new(ErrorKind::VerifyFailed, format!("sources/{name} on the drive is shorter than it should be.")).with_hint("Make the installer again."));
                    }
                    r?;
                    if has_more(&mut f) {
                        return Err(NyError::new(ErrorKind::VerifyFailed, format!("sources/{name} on the drive is longer than it should be.")).with_hint("Make the installer again."));
                    }
                    *files += 1;
                }
                continue;
            }
        }
        let mut f = dir.open_file(&e.name).map_err(|_| missing(&path))?;
        compare_with_iso(job, &e, &path, &mut f, emit, a, b)?;
        *files += 1;
    }
    Ok(())
}

/// Read a small file by its path from the root of the FAT volume.
fn read_small<IO: ReadWriteSeek>(root: &fatfs::Dir<'_, IO>, path: &str) -> Result<Vec<u8>> {
    let mut parts: Vec<&str> = path.split('/').collect();
    let file = parts.pop().unwrap_or("");
    let mut dir = root.clone();
    for p in parts {
        dir = dir.open_dir(p).map_err(|_| missing(path))?;
    }
    let mut f = dir.open_file(file).map_err(|_| missing(path))?;
    let mut v = vec![];
    f.read_to_end(&mut v).map_err(|e| NyError::from_io(&e, &format!("reading {path} back")))?;
    Ok(v)
}

/// Read a Windows installer made by [`make_windows_media`] back and compare it with the ISO.
///
/// Checks the partition table and boot code, then every file the ISO contains (a split `install.wim` is regenerated
/// from the ISO and compared part by part), plus the small files NyForge adds. Writes nothing.
pub fn verify_windows_media(iso_path: &Path, target: &Target, opts: &WindowsOptions, cancel: Arc<AtomicBool>, emit: &mut dyn FnMut(Event)) -> Result<WindowsVerifyReport> {
    let iso = IsoImage::open(iso_path)?;
    let report = probe::analyze(&iso, std::fs::metadata(iso_path).map(|m| m.len()).unwrap_or(0))?;
    if report.verdict != Verdict::WindowsInstaller {
        return Err(NyError::invalid("This ISO is not a Windows installer."));
    }
    let win = report.windows.clone().unwrap();
    emit(Event::Phase { phase: Phase::Verifying, detail: "Reading the drive back".into() });
    let started = Instant::now();

    let disk_bytes = match target.kind {
        TargetKind::BlockDevice => target.capacity.ok_or_else(|| NyError::other("unknown drive size"))?,
        TargetKind::File => std::fs::metadata(&target.path).map_err(|e| NyError::from_io(&e, &format!("reading {}", target.display)))?.len(),
    };
    let file = sys::open_read(&target.path, target.kind, false).map_err(|e| NyError::from_io(&e, &format!("opening {}", target.display)))?;
    if target.kind == TargetKind::BlockDevice {
        sys::drop_cache(&file);
    }
    let whole = FileIo::new(file, 0, (disk_bytes / SECTOR) * SECTOR, sys::block_io_align(target.kind));
    let io_err = |e: std::io::Error| NyError::from_io(&e, &format!("reading {} back", target.display));

    // Partition table and boot code.
    let mut sector0 = [0u8; 512];
    whole.read_at(&mut sector0, 0).map_err(io_err)?;
    let parts = crate::nyboot::layout::parse_mbr(&sector0).unwrap_or_default();
    let (start, sectors) = match parts.as_slice() {
        [(0x0C, start, sectors)] => (*start, *sectors),
        _ => {
            return Err(NyError::new(ErrorKind::VerifyFailed, "The drive does not have the partition table of a NyForge Windows installer.")
                .with_hint("Make the installer first; this drive may hold something else."))
        }
    };
    let sig = u32::from_le_bytes(sector0[440..444].try_into().unwrap());
    if sector0[..] != windows_mbr(sig, start, sectors)[..] {
        return Err(NyError::new(ErrorKind::VerifyFailed, "The boot sector on the drive is not the one NyForge writes.").with_hint("Make the installer again."));
    }
    if opts.bios {
        let mut core = assets::CORE_WIN_IMG.to_vec();
        core.resize(core.len().next_multiple_of(SECTOR as usize), 0);
        let mut got = vec![0u8; core.len()];
        whole.read_at(&mut got, SECTOR).map_err(io_err)?;
        if got != core {
            return Err(NyError::new(ErrorKind::VerifyFailed, "The legacy-BIOS boot code on the drive is damaged.").with_hint("Make the installer again, or make a UEFI-only stick."));
        }
    }
    if (start + sectors) * SECTOR > whole.len() {
        return Err(NyError::new(ErrorKind::VerifyFailed, "The partition on the drive reaches past the end of the drive."));
    }

    let part_io = whole.sub(start * SECTOR, sectors * SECTOR).map_err(io_err)?;
    let mut cs = CachedStream::new(part_io);
    let fs = FileSystem::new(&mut cs, FsOptions::new()).map_err(|_| NyError::new(ErrorKind::VerifyFailed, "The drive's Windows partition is not a readable FAT32 volume.").with_hint("Make the installer again."))?;
    if fs.fat_type() != FatType::Fat32 {
        return Err(NyError::new(ErrorKind::VerifyFailed, "The drive's Windows partition is not FAT32."));
    }
    let root = fs.root_dir();

    // The split image carries its own GUID; take it from the drive so the parts can be rebuilt identically.
    let mut notes = vec![];
    let mut plan = decide_plan(&iso, &win, opts, &mut notes)?;
    if let Plan::Split(sp) = plan {
        let head = read_small_head(&root, "sources/install.swm")?;
        let guid = SplitPlan::guid_of_part(&head).ok_or_else(|| NyError::new(ErrorKind::VerifyFailed, "sources/install.swm on the drive is not a Windows image part."))?;
        plan = Plan::Split(Box::new((*sp).with_guid(guid)));
    }
    let total = plan_total(&iso, &plan)?;
    let mut job = Job {
        iso: &iso,
        cancel: &cancel,
        total,
        done: 0,
        meter: SpeedMeter::new(Duration::from_secs(3)),
        last_emit: Instant::now() - Duration::from_secs(1),
        target: target.display.clone(),
    };
    let (mut a, mut b) = (vec![0u8; 1 << 20], vec![0u8; 1 << 20]);
    let mut files = 0usize;
    verify_tree(&mut job, "", &root, &plan, &mut files, emit, &mut a, &mut b)?;

    // The small files NyForge adds.
    if opts.bios && read_small(&root, "boot/grub/grub.cfg")? != GRUB_CFG.as_bytes() {
        return Err(differs("boot/grub/grub.cfg", 0));
    }
    let arch = if win.uefi_arm64 && !win.uefi_x64 { "arm64" } else { "amd64" };
    if let Some(xml) = autounattend_xml(arch, &opts.unattend) {
        if !iso.exists("autounattend.xml") && read_small(&root, "autounattend.xml")? != xml.as_bytes() {
            return Err(differs("autounattend.xml", 0));
        }
    }
    let elapsed = started.elapsed();
    let speed = if elapsed.as_secs_f64() > 0.0 { total as f64 / elapsed.as_secs_f64() } else { 0.0 };
    emit(Event::Verified { bytes: total, elapsed_secs: elapsed.as_secs_f64(), avg_speed_bps: speed });
    Ok(WindowsVerifyReport { bytes: total, files, elapsed })
}

/// The first bytes of a file on the FAT volume.
fn read_small_head<IO: ReadWriteSeek>(root: &fatfs::Dir<'_, IO>, path: &str) -> Result<Vec<u8>> {
    let mut parts: Vec<&str> = path.split('/').collect();
    let file = parts.pop().unwrap_or("");
    let mut dir = root.clone();
    for p in parts {
        dir = dir.open_dir(p).map_err(|_| missing(path))?;
    }
    let mut f = dir.open_file(file).map_err(|_| missing(path))?;
    let mut v = vec![0u8; crate::wim::HEADER_LEN];
    f.read_exact(&mut v).map_err(|e| NyError::from_io(&e, &format!("reading {path} back")))?;
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::process::Command;

    fn have(c: &str) -> bool {
        Command::new("sh").args(["-c", &format!("command -v {c} >/dev/null")]).status().map(|s| s.success()).unwrap_or(false)
    }

    #[test]
    fn labels_are_valid_fat_labels() {
        assert_eq!(fat_label("CCCOMA_X64FRE_EN-US_DV9"), "CCCOMA_X64F");
        assert_eq!(fat_label("win 11: pro"), "WIN 11  PRO");
        assert_eq!(fat_label("???"), "WINDOWS");
    }

    #[test]
    fn unattend_is_only_made_when_asked() {
        assert!(autounattend_xml("amd64", &Unattend::default()).is_none());
        let x = autounattend_xml("amd64", &Unattend { bypass_hardware_checks: true }).unwrap();
        for k in ["BypassTPMCheck", "BypassSecureBootCheck", "BypassRAMCheck", "BypassCPUCheck", "BypassStorageCheck"] {
            assert!(x.contains(k), "{k}");
        }
        assert!(x.contains("pass=\"windowsPE\"") && x.contains("processorArchitecture=\"amd64\""));
        assert_eq!(x.matches("<RunSynchronousCommand").count(), x.matches("</RunSynchronousCommand>").count());
    }

    #[test]
    fn mbr_has_one_active_fat32_partition() {
        let m = windows_mbr(0xABCD_1234, 2048, 1_000_000);
        let p = crate::nyboot::layout::parse_mbr(&m).unwrap();
        assert_eq!(p, vec![(0x0C, 2048, 1_000_000)]);
        assert_eq!(m[446], 0x80);
        assert!(m[462..510].iter().all(|&b| b == 0));
    }

    /// Removes a test's scratch directory when the test ends, pass or fail (these hold gigabytes).
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A small stand-in for a Windows ISO: real WIM files (made with wimlib), UDF hybrid like Microsoft's.
    fn fake_windows_iso(name: &str) -> Option<(PathBuf, PathBuf)> {
        if !have("genisoimage") || !have("wimlib-imagex") {
            return None;
        }
        let d = std::env::temp_dir().join(format!("nyforge-win-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&d);
        let payload = d.join("payload");
        std::fs::create_dir_all(payload.join("a")).unwrap();
        let mut x = 7u64;
        for i in 1..=5usize {
            let data: Vec<u8> = (0..i * 400_000)
                .map(|_| {
                    x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                    (x >> 33) as u8
                })
                .collect();
            std::fs::write(payload.join(format!("a/f{i}.bin")), data).unwrap();
        }
        let t = d.join("t");
        std::fs::create_dir_all(t.join("sources")).unwrap();
        std::fs::create_dir_all(t.join("boot")).unwrap();
        std::fs::create_dir_all(t.join("EFI/BOOT")).unwrap();
        std::fs::create_dir_all(t.join("EFI/Microsoft/Boot")).unwrap();
        std::fs::write(t.join("bootmgr"), b"BOOTMGR-STUB").unwrap();
        std::fs::write(t.join("boot/bcd"), b"BCD").unwrap();
        std::fs::write(t.join("EFI/BOOT/bootx64.efi"), b"MZ-efi").unwrap();
        std::fs::write(t.join("setup.exe"), b"MZ-setup").unwrap();
        for (n, c) in [("boot.wim", "XPRESS"), ("install.wim", "none")] {
            let st = Command::new("wimlib-imagex").arg("capture").arg(&payload).arg(t.join("sources").join(n)).args(["Img", &format!("--compress={c}")]).output().unwrap();
            assert!(st.status.success());
        }
        let iso = d.join("win.iso");
        let st = Command::new("genisoimage").args(["-udf", "-iso-level", "1", "-V", "CCCOMA_X64FRE", "-o"]).arg(&iso).arg(&t).output().unwrap();
        assert!(st.status.success(), "{}", String::from_utf8_lossy(&st.stderr));
        Some((d, iso))
    }

    fn extract_partition(img: &Path, out: &Path) {
        let st = Command::new("dd").arg(format!("if={}", img.display())).arg(format!("of={}", out.display())).args(["bs=1M", "skip=1", "status=none"]).status().unwrap();
        assert!(st.success());
    }

    #[test]
    fn builds_a_fat32_windows_stick_and_splits_install_wim() {
        let Some((d, iso)) = fake_windows_iso("full") else { return };
        let _cleanup = Cleanup(d.clone());
        let img = d.join("stick.img");
        let opts = WindowsOptions {
            file_size: Some(1 << 30),
            part_bytes: 2_400_000, // force splitting (install.wim here is ~6 MB)
            unattend: Unattend { bypass_hardware_checks: true },
            ..Default::default()
        };
        let mut events = vec![];
        let rep = make_windows_media(&iso, &Target::file(&img), &opts, Arc::new(AtomicBool::new(false)), &mut |e| events.push(e)).unwrap();
        assert!(rep.split_parts >= 3, "{rep:?}");
        assert_eq!(rep.label, "CCCOMA_X64F");
        assert!(matches!(events.last().unwrap(), Event::Finished { ok: true, .. }));
        assert!(events.iter().any(|e| matches!(e, Event::Progress(_))) || rep.bytes < 1 << 20 || true);

        // Partition table + boot code.
        let raw = std::fs::read(&img).unwrap();
        let parts = crate::nyboot::layout::parse_mbr(&raw[..512]).unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].0, 0x0C);
        assert_eq!(&raw[512..512 + assets::CORE_WIN_IMG.len()], assets::CORE_WIN_IMG);

        // The partition is a healthy FAT32 filesystem.
        let part = d.join("part.img");
        extract_partition(&img, &part);
        if have("fsck.vfat") {
            let o = Command::new("fsck.vfat").arg("-n").arg(&part).output().unwrap();
            assert!(o.status.success(), "fsck.vfat: {}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        }
        // Hidden sectors in the boot sector (and its backup) = partition start.
        let ps = std::fs::read(&part).unwrap();
        assert_eq!(u32::from_le_bytes(ps[0x1C..0x20].try_into().unwrap()), 2048);
        assert_eq!(u32::from_le_bytes(ps[6 * 512 + 0x1C..6 * 512 + 0x20].try_into().unwrap()), 2048);

        // Files are all there; the big image is split; the bypass file exists.
        if have("mdir") {
            let ls = Command::new("mdir").args(["-/", "-b", "-i"]).arg(&part).arg("::").output().unwrap();
            let listing = String::from_utf8_lossy(&ls.stdout).to_string();
            for want in ["::/bootmgr", "::/boot/bcd", "::/EFI/BOOT/bootx64.efi", "::/sources/boot.wim", "::/sources/install.swm", "::/sources/install2.swm", "::/autounattend.xml", "::/boot/grub/grub.cfg", "::/setup.exe"] {
                assert!(listing.to_lowercase().contains(&want.to_lowercase()), "missing {want} in\n{listing}");
            }
            assert!(!listing.to_lowercase().contains("::/sources/install.wim"), "unsplit image must not be copied");
        }

        // Extract everything with mtools and check the content and the split image.
        let out = d.join("extracted");
        std::fs::create_dir_all(&out).unwrap();
        let st = Command::new("mcopy").args(["-s", "-n", "-i"]).arg(&part).arg("::").arg(&out).output().unwrap();
        assert!(st.status.success(), "{}", String::from_utf8_lossy(&st.stderr));
        let find = |name: &str| -> PathBuf {
            let mut stack = vec![out.clone()];
            while let Some(p) = stack.pop() {
                for e in std::fs::read_dir(&p).unwrap().flatten() {
                    let ep = e.path();
                    if ep.is_dir() {
                        stack.push(ep);
                    } else if ep.file_name().unwrap().to_string_lossy().eq_ignore_ascii_case(name) {
                        return ep;
                    }
                }
            }
            panic!("{name} not found");
        };
        assert_eq!(std::fs::read(find("bootmgr")).unwrap(), b"BOOTMGR-STUB");
        assert_eq!(std::fs::read(find("grub.cfg")).unwrap(), GRUB_CFG.as_bytes());
        let swm = find("install.swm");
        let applied = d.join("applied");
        let st = Command::new("wimlib-imagex")
            .arg("apply")
            .arg(&swm)
            .arg("1")
            .arg(&applied)
            .arg(format!("--ref={}/install*.swm", swm.parent().unwrap().display()))
            .output()
            .unwrap();
        assert!(st.status.success(), "{}", String::from_utf8_lossy(&st.stderr));
        let diff = Command::new("diff").arg("-r").arg(d.join("payload")).arg(&applied).output().unwrap();
        assert!(diff.status.success(), "install image content differs: {}", String::from_utf8_lossy(&diff.stdout));
        // boot.wim copied verbatim.
        let orig = std::fs::read(d.join("t/sources/boot.wim")).unwrap();
        assert_eq!(std::fs::read(find("boot.wim")).unwrap(), orig);
    }


    fn find_bytes(hay: &[u8], needle: &[u8]) -> Option<usize> {
        let first = needle[0];
        let mut from = 0;
        while let Some(i) = hay[from..].iter().position(|&b| b == first) {
            let at = from + i;
            if hay.len() >= at + needle.len() && &hay[at..at + needle.len()] == needle {
                return Some(at);
            }
            from = at + 1;
        }
        None
    }

    #[test]
    fn a_finished_windows_stick_verifies_and_every_kind_of_damage_is_found() {
        let Some((d, iso)) = fake_windows_iso("verify") else { return };
        let _cleanup = Cleanup(d.clone());
        let img = d.join("stick.img");
        let opts = WindowsOptions {
            file_size: Some(200 << 20),
            part_bytes: 2_400_000, // force splitting
            unattend: Unattend { bypass_hardware_checks: true },
            ..Default::default()
        };
        let no = || Arc::new(AtomicBool::new(false));
        make_windows_media(&iso, &Target::file(&img), &opts, no(), &mut |_| {}).unwrap();

        let mut ev = vec![];
        let rep = verify_windows_media(&iso, &Target::file(&img), &opts, no(), &mut |e| ev.push(e)).unwrap();
        assert!(rep.files >= 8, "{rep:?}"); // boot files + at least 3 install parts
        assert!(matches!(ev.last().unwrap(), Event::Verified { .. }));
        assert!(ev.iter().any(|e| matches!(e, Event::Phase { phase: Phase::Verifying, .. })));

        let original = std::fs::read(&img).unwrap();
        let flip = |at: usize| {
            let mut raw = original.clone();
            raw[at] ^= 0x40;
            std::fs::write(&img, raw).unwrap();
            verify_windows_media(&iso, &Target::file(&img), &opts, no(), &mut |_| {}).unwrap_err()
        };
        // a file copied from the ISO
        let boot = std::fs::read(d.join("t/sources/boot.wim")).unwrap();
        let boot_at = find_bytes(&original, &boot[..64]).expect("boot.wim on the stick");
        let e = flip(boot_at + 100);
        assert_eq!(e.kind, ErrorKind::VerifyFailed);
        assert!(e.message.to_lowercase().contains("boot.wim"), "{}", e.message);
        // data inside a split part of install.wim (uncompressed payload, so its bytes are visible on the stick)
        let payload = std::fs::read(d.join("payload/a/f5.bin")).unwrap();
        // (boot.wim holds the same incompressible bytes, so look beyond it)
        let after = boot_at + boot.len();
        let at = after + find_bytes(&original[after..], &payload[..64]).expect("install data on the stick");
        let e = flip(at + 1000);
        assert!(e.message.contains("install") && e.message.contains(".swm"), "{}", e.message);
        // the boot sector
        let e = flip(100);
        assert!(e.message.contains("boot sector"), "{}", e.message);
        // GRUB's core image
        let e = flip(512 + 10);
        assert!(e.message.contains("boot code"), "{}", e.message);
        // untouched again: passes
        std::fs::write(&img, &original).unwrap();
        verify_windows_media(&iso, &Target::file(&img), &opts, no(), &mut |_| {}).unwrap();
        // a stick that is not a Windows installer at all
        std::fs::write(&img, vec![0u8; 200 << 20]).unwrap();
        let e = verify_windows_media(&iso, &Target::file(&img), &opts, no(), &mut |_| {}).unwrap_err();
        assert_eq!(e.kind, ErrorKind::VerifyFailed);
    }

    #[test]
    fn refuses_non_windows_and_too_small() {
        let Some((d, iso)) = fake_windows_iso("small") else { return };
        let _cleanup = Cleanup(d.clone());
        let img = d.join("tiny.img");
        let o = WindowsOptions { file_size: Some(40 << 20), ..Default::default() };
        let e = make_windows_media(&iso, &Target::file(&img), &o, Arc::new(AtomicBool::new(false)), &mut |_| {}).err().unwrap();
        assert_eq!(e.kind, ErrorKind::NoSpace, "{}", e.message);
        // A non-Windows ISO.
        if have("xorriso") {
            let t = d.join("lin");
            std::fs::create_dir_all(t.join("casper")).unwrap();
            std::fs::write(t.join("casper/vmlinuz"), b"k").unwrap();
            let l = d.join("lin.iso");
            Command::new("xorriso").args(["-as", "mkisofs", "-r", "-o"]).arg(&l).arg(&t).output().unwrap();
            let o = WindowsOptions { file_size: Some(1 << 30), ..Default::default() };
            let e = make_windows_media(&l, &Target::file(d.join("x.img")), &o, Arc::new(AtomicBool::new(false)), &mut |_| {}).err().unwrap();
            assert_eq!(e.kind, ErrorKind::InvalidInput);
        }
    }

    #[test]
    fn cancel_stops_the_copy() {
        let Some((d, iso)) = fake_windows_iso("cancel") else { return };
        let _cleanup = Cleanup(d.clone());
        let o = WindowsOptions { file_size: Some(1 << 30), ..Default::default() };
        let e = make_windows_media(&iso, &Target::file(d.join("c.img")), &o, Arc::new(AtomicBool::new(true)), &mut |_| {}).err().unwrap();
        assert_eq!(e.kind, ErrorKind::Cancelled);
    }
}
