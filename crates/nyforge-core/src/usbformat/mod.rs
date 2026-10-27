//! `nyforge format`: wipe and re-format a USB drive or SD card.
//!
//! What it does that a plain "format" button does not:
//!
//! * **removable media only** — enforced here, in the engine, not in the command line parser: system disks,
//!   internal disks, virtual devices and read-only media are refused no matter how the call is made, and the
//!   drive is looked up again right before the first byte is written;
//! * FAT16 / FAT32 / exFAT written natively (any size, any platform), ext4 and NTFS through the system's tools (Linux: `mkfs.ext4`, `mkntfs`; Windows: NTFS through `Format-Volume`);
//! * MBR, GPT or no partition table at all ("superfloppy", for cameras and TVs), with several partitions in one go;
//! * quick, zero or random wipe;
//! * a **counterfeit-capacity test** (quick sampling or exact), and `--fit-real-capacity` to use only the part that works.

pub mod capacity;
pub mod fsmake;
pub mod prng;
pub mod tables;

#[cfg(test)]
mod tests;

use crate::device::{self, Bus, Drive, Safety};
use crate::error::{ErrorKind, NyError, Result};
use crate::format::{human_size, round_up};
use crate::nyboot::io::{FileIo, Io};
use crate::progress::{Event, Phase, Progress, SpeedMeter, TargetProgress, TargetState};
use crate::sys::{self, TargetKind};
use crate::winnative;
use crate::writer::Target;
use capacity::{CapacityReport, CapacityTest};
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tables::{TablePart, MIB_SECTORS, SECTOR};

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;
const TIB: u64 = 1 << 40;

// ------------------------------------------------------------------------------ types

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fs {
    Fat16,
    Fat32,
    #[serde(rename = "exfat")]
    ExFat,
    Ntfs,
    Ext2,
    Ext3,
    Ext4,
}

impl Fs {
    pub fn name(self) -> &'static str {
        match self {
            Fs::Fat16 => "FAT16",
            Fs::Fat32 => "FAT32",
            Fs::ExFat => "exFAT",
            Fs::Ntfs => "NTFS",
            Fs::Ext2 => "ext2",
            Fs::Ext3 => "ext3",
            Fs::Ext4 => "ext4",
        }
    }

    /// ext2, ext3 and ext4: made by the same system tool, and owned by a Linux user rather than "everyone".
    pub fn is_ext(self) -> bool {
        matches!(self, Fs::Ext2 | Fs::Ext3 | Fs::Ext4)
    }

    pub fn min_bytes(self) -> u64 {
        match self {
            Fs::Fat16 => 8 * MIB,
            Fs::Fat32 => 33 * MIB,
            Fs::ExFat => 8 * MIB,
            Fs::Ntfs | Fs::Ext2 | Fs::Ext3 | Fs::Ext4 => 16 * MIB,
        }
    }

    pub fn max_bytes(self) -> Option<u64> {
        match self {
            Fs::Fat16 => Some(2 * GIB),
            Fs::Fat32 => Some(2 * TIB - MIB),
            _ => None,
        }
    }

    pub fn mbr_type(self, sectors: u64) -> u8 {
        match self {
            Fs::Fat16 if sectors < 65536 => 0x04,
            Fs::Fat16 => 0x0E,
            Fs::Fat32 => 0x0C,
            Fs::ExFat | Fs::Ntfs => 0x07,
            Fs::Ext2 | Fs::Ext3 | Fs::Ext4 => 0x83,
        }
    }

    pub fn gpt_type(self) -> tables::Guid {
        match self {
            Fs::Ext2 | Fs::Ext3 | Fs::Ext4 => tables::type_linux(),
            _ => tables::type_basic_data(),
        }
    }
}

impl FromStr for Fs {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "fat16" => Ok(Fs::Fat16),
            "fat32" | "fat" | "vfat" => Ok(Fs::Fat32),
            "exfat" => Ok(Fs::ExFat),
            "ntfs" => Ok(Fs::Ntfs),
            "ext4" | "ext" => Ok(Fs::Ext4),
            "ext3" => Ok(Fs::Ext3),
            "ext2" => Ok(Fs::Ext2),
            other => Err(format!("\"{other}\" is not a filesystem NyForge can make (fat16, fat32, exfat, ntfs, ext2, ext3, ext4).")),
        }
    }
}

/// Can this machine make `fs`? FAT16/FAT32/exFAT always; ext4 and NTFS need the system's mkfs (Linux).
pub fn can_make(fs: Fs) -> bool {
    !fsmake::needs_tool(fs) || fsmake::check_tool(fs).is_ok()
}

/// The filesystem to use when you do not say: FAT32 reads everywhere and is best up to 32 GiB
/// (Windows' own formatter refuses bigger FAT32), exFAT beyond that.
pub fn auto_fs(disk_bytes: u64) -> Fs {
    if disk_bytes <= 32 * GIB {
        Fs::Fat32
    } else {
        Fs::ExFat
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scheme {
    Mbr,
    Gpt,
    /// No partition table: the filesystem starts at the very first sector.
    None,
}

impl Scheme {
    pub fn name(self) -> &'static str {
        match self {
            Scheme::Mbr => "MBR",
            Scheme::Gpt => "GPT",
            Scheme::None => "no partition table",
        }
    }
}

impl FromStr for Scheme {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "mbr" | "dos" | "msdos" => Ok(Scheme::Mbr),
            "gpt" => Ok(Scheme::Gpt),
            "none" | "superfloppy" => Ok(Scheme::None),
            other => Err(format!("\"{other}\" is not a partition scheme (mbr, gpt, none).")),
        }
    }
}

/// MBR works on the most devices (BIOS, TVs, cameras, car stereos) up to 2 TiB; GPT beyond.
pub fn auto_scheme(disk_bytes: u64) -> Scheme {
    if disk_bytes <= 2 * TIB {
        Scheme::Mbr
    } else {
        Scheme::Gpt
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Wipe {
    /// Clear the old partition tables and filesystem signatures (seconds).
    Quick,
    /// Overwrite every byte with zeros.
    Zero,
    /// Overwrite every byte with random data.
    Random,
}

impl FromStr for Wipe {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "quick" => Ok(Wipe::Quick),
            "zero" | "zeros" => Ok(Wipe::Zero),
            "random" => Ok(Wipe::Random),
            other => Err(format!("\"{other}\" is not a wipe mode (quick, zero, random).")),
        }
    }
}

/// One partition to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartSpec {
    pub fs: Fs,
    /// `None` = everything that is left (only allowed for the last partition).
    pub size: Option<u64>,
    pub label: String,
    pub cluster: Option<u64>,
}

impl FromStr for PartSpec {
    type Err = String;
    /// `fs[:size[:label[:cluster]]]`, e.g. `fat32:16G:DATA`, `ext4::LINUX`, `exfat`.
    fn from_str(s: &str) -> std::result::Result<Self, String> {
        let mut it = s.split(':');
        let fs: Fs = it.next().unwrap_or("").parse()?;
        let size = match it.next() {
            None | Some("") | Some("rest") => None,
            Some(v) => Some(crate::format::parse_size(v).ok_or_else(|| format!("\"{v}\" is not a size like 16G or 512M."))?),
        };
        let label = it.next().unwrap_or("").to_string();
        let cluster = match it.next() {
            None | Some("") => None,
            Some(v) => Some(crate::format::parse_size(v).ok_or_else(|| format!("\"{v}\" is not a cluster size like 32K."))?),
        };
        if it.next().is_some() {
            return Err("A partition is written fs[:size[:label[:cluster]]].".into());
        }
        Ok(PartSpec { fs, size, label, cluster })
    }
}

/// Who does the formatting on Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Engine {
    /// Windows' own tools for a plain quick format (they are the most compatible with unusual sticks), NyForge's
    /// own writer for everything else and whenever Windows' tools do not manage. Elsewhere: NyForge's own writer.
    #[default]
    Auto,
    /// Windows' own tools only: fails if the request is something they cannot do.
    Windows,
    /// NyForge's own writer only.
    Own,
}

impl FromStr for Engine {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Engine::Auto),
            "windows" | "native" | "system" => Ok(Engine::Windows),
            "own" | "raw" | "nyforge" => Ok(Engine::Own),
            other => Err(format!("\"{other}\" is not an engine (auto, windows, own).")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct FormatOptions {
    pub parts: Vec<PartSpec>,
    pub scheme: Scheme,
    pub wipe: Wipe,
    pub capacity_test: CapacityTest,
    /// After a failed full capacity test, format only the part of the drive that works.
    pub fit_real_capacity: bool,
    /// How many times the full test writes and reads the drive (1 to 4). Each pass uses new data, like a bad-blocks check.
    pub passes: u8,
    /// Leave this much unpartitioned at the end.
    pub reserve_bytes: u64,
    /// Required when the target is an image file.
    pub file_size: Option<u64>,
    pub unmount: bool,
    /// Owner (uid, gid) of an ext4 root directory; otherwise only root could write to the stick.
    pub owner: Option<(u32, u32)>,
    /// Windows only: who does the formatting (see [`Engine`]).
    pub engine: Engine,
}

impl Default for FormatOptions {
    fn default() -> Self {
        FormatOptions {
            parts: vec![],
            scheme: Scheme::Mbr,
            wipe: Wipe::Quick,
            capacity_test: CapacityTest::Off,
            fit_real_capacity: false,
            passes: 1,
            reserve_bytes: 0,
            file_size: None,
            unmount: true,
            owner: None,
            engine: Engine::Auto,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartitionReport {
    pub number: u32,
    pub start_bytes: u64,
    pub size_bytes: u64,
    pub fs: Fs,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormatReport {
    pub target: String,
    pub scheme: Scheme,
    pub disk_bytes: u64,
    /// How much of the drive was used (less than `disk_bytes` with MBR beyond 2 TiB or --fit-real-capacity).
    pub usable_bytes: u64,
    pub partitions: Vec<PartitionReport>,
    pub wipe: Wipe,
    pub capacity: Option<CapacityReport>,
    pub elapsed_secs: f64,
    pub notes: Vec<String>,
}

// ---------------------------------------------------------------------------- planning

#[derive(Debug, Clone)]
pub struct PartPlan {
    pub number: u32,
    pub start: u64,
    pub sectors: u64,
    pub spec: PartSpec,
}

impl PartPlan {
    pub fn bytes(&self) -> u64 {
        self.sectors * SECTOR
    }
}

/// Work out where every partition goes. Pure arithmetic: nothing is touched, so bad requests fail before anything is erased.
pub fn plan_layout(disk_bytes: u64, scheme: Scheme, specs: &[PartSpec], reserve_bytes: u64) -> Result<Vec<PartPlan>> {
    if specs.is_empty() {
        return Err(NyError::invalid("Nothing to create: no partition was requested."));
    }
    let disk_sectors = disk_bytes / SECTOR;
    match scheme {
        Scheme::None if specs.len() != 1 => return Err(NyError::invalid("Without a partition table there can be only one filesystem.")),
        Scheme::Mbr if specs.len() > 4 => return Err(NyError::invalid("An MBR drive holds at most 4 partitions. Use --scheme gpt for more.")),
        Scheme::Gpt if specs.len() > 16 => return Err(NyError::invalid("That is more partitions than makes sense (16 at most).")),
        _ => {}
    }
    for (i, s) in specs.iter().enumerate() {
        if s.size.is_none() && i + 1 != specs.len() {
            return Err(NyError::invalid("Only the last partition can take \"the rest\" of the drive: give the others a size."));
        }
    }
    let reserve = round_up(reserve_bytes, MIB) / SECTOR;
    let first = if scheme == Scheme::None { 0 } else { MIB_SECTORS };
    let end = match scheme {
        Scheme::None => disk_sectors,
        Scheme::Mbr => disk_sectors.min(1 << 32) & !(MIB_SECTORS - 1),
        Scheme::Gpt => disk_sectors.saturating_sub(tables::GPT_TAIL_SECTORS + 1) & !(MIB_SECTORS - 1),
    };
    let end = end.saturating_sub(reserve);
    let mut cursor = first;
    let mut out = vec![];
    for (i, s) in specs.iter().enumerate() {
        let sectors = match s.size {
            Some(b) => round_up(b.max(1), MIB) / SECTOR,
            None => end.saturating_sub(cursor),
        };
        if cursor + sectors > end {
            return Err(NyError::invalid(format!(
                "Partition {} ({}) needs {} but only {} of the drive is left.",
                i + 1,
                s.fs.name(),
                human_size(sectors * SECTOR),
                human_size(end.saturating_sub(cursor) * SECTOR)
            ))
            .with_hint("Make the partitions smaller, or leave the last one without a size to take the rest."));
        }
        let bytes = sectors * SECTOR;
        if bytes < s.fs.min_bytes() {
            return Err(NyError::invalid(format!("{} needs at least {} (partition {} would have {}).", s.fs.name(), human_size(s.fs.min_bytes()), i + 1, human_size(bytes))));
        }
        if let Some(max) = s.fs.max_bytes() {
            if bytes > max {
                return Err(NyError::invalid(format!(
                    "{} can be at most {} (partition {} would have {}).",
                    s.fs.name(),
                    human_size(max),
                    i + 1,
                    human_size(bytes)
                ))
                .with_hint(if s.fs == Fs::Fat16 { "Use FAT32 or exFAT for bigger partitions." } else { "Use exFAT for bigger partitions." }));
            }
        }
        out.push(PartPlan { number: i as u32 + 1, start: cursor, sectors, spec: s.clone() });
        cursor += sectors;
    }
    Ok(out)
}

// -------------------------------------------------------------------------------- safety

/// Mount points that mean "this is somebody's operating system".
const SYSTEM_MOUNTS: &[&str] = &[
    // Linux
    "/", "/boot", "/boot/efi", "/efi", "/home", "/usr", "/var", "/etc", "/opt",
    // macOS
    "/System", "/System/Volumes/Data", "/Users", "/Library", "/Applications", "/private",
    // Windows
    "C:\\",
];

/// The rule behind "format never touches PC drives": only USB sticks/disks and SD cards qualify,
/// and only when nothing marks them as part of the running system.
pub fn check_drive_allowed(d: &Drive) -> Result<()> {
    match d.safety {
        Safety::System => {
            return Err(NyError::new(ErrorKind::Unsafe, format!("{} ({}) holds the running system. NyForge will never format it.", d.path, d.display_name())));
        }
        Safety::Internal => {
            return Err(NyError::new(
                ErrorKind::Unsafe,
                format!("{} ({}, {}) is a fixed internal disk. NyForge only formats removable USB drives and SD cards.", d.path, d.display_name(), human_size(d.size)),
            ));
        }
        Safety::Virtual => {
            return Err(NyError::new(ErrorKind::Unsafe, format!("{} is a virtual or loop device, not a USB drive or SD card.", d.path)));
        }
        Safety::Safe => {}
    }
    if !matches!(d.bus, Bus::Usb | Bus::Sd) {
        return Err(NyError::new(
            ErrorKind::Unsafe,
            format!("{} ({}) is not connected over USB or an SD slot, so NyForge does not treat it as removable.", d.path, d.display_name()),
        ));
    }
    if let Some(m) = d.mounted_points().into_iter().find(|m| SYSTEM_MOUNTS.contains(m)) {
        return Err(NyError::new(ErrorKind::Unsafe, format!("{} is mounted at {m}, which looks like part of an operating system.", d.path)));
    }
    if d.read_only {
        return Err(NyError::new(ErrorKind::ReadOnly, format!("{} is write-protected.", d.path)).with_hint("Check for a lock switch on the drive or SD adapter."));
    }
    Ok(())
}

/// The drives `format` may offer: removable USB and SD media, nothing else.
pub fn formattable_drives(all: Vec<Drive>) -> Vec<Drive> {
    all.into_iter().filter(|d| d.size > 0 && check_drive_allowed(d).is_ok()).collect()
}

pub fn list_formattable() -> Result<Vec<Drive>> {
    Ok(formattable_drives(device::list_drives()?))
}

/// Find the drive named by `spec` and apply the format rule to it. There is no way to relax the rule.
///
/// The generic resolver is asked in its most permissive mode only so that *this* module can refuse with its own
/// words (and without pointing at `--allow-…` flags that `format` does not have). The system disk is refused by the
/// generic resolver regardless of policy, and everything else goes through `check_drive_allowed` right here,
/// and again inside `format()`.
pub fn resolve_target(spec: &str) -> Result<Target> {
    let r = device::resolve_target(spec, device::TargetPolicy { allow_internal: true, allow_virtual: true })?;
    let t = Target::from_resolved(r);
    if let Some(d) = &t.drive {
        check_drive_allowed(d)?;
    }
    Ok(t)
}

// ------------------------------------------------------------------------------ progress

struct Reporter<'a> {
    emit: &'a mut dyn FnMut(Event),
    target: String,
    meter: SpeedMeter,
    last: Instant,
    started: Instant,
}

impl<'a> Reporter<'a> {
    fn new(emit: &'a mut dyn FnMut(Event), target: String) -> Self {
        Reporter { emit, target, meter: SpeedMeter::new(Duration::from_secs(3)), last: Instant::now() - Duration::from_secs(1), started: Instant::now() }
    }

    fn log(&mut self, msg: impl Into<String>) {
        (self.emit)(Event::Log { message: msg.into() });
    }

    fn phase(&mut self, phase: Phase, detail: impl Into<String>) {
        (self.emit)(Event::Phase { phase, detail: detail.into() });
    }

    /// A new stretch of work with its own 0-100 %.
    fn restart(&mut self) {
        self.meter = SpeedMeter::new(Duration::from_secs(3));
        self.last = Instant::now() - Duration::from_secs(1);
    }

    fn progress(&mut self, done: u64, total: u64) {
        let now = Instant::now();
        if now.duration_since(self.last) < Duration::from_millis(250) && done < total {
            return;
        }
        self.last = now;
        let speed = self.meter.sample(now, done);
        let left = total.saturating_sub(done);
        (self.emit)(Event::Progress(Progress {
            bytes_done: done,
            bytes_total: Some(total),
            fraction: if total == 0 { 1.0 } else { (done as f64 / total as f64).min(1.0) },
            speed_bps: speed,
            eta_secs: if speed > 1.0 { Some(left as f64 / speed) } else { None },
            targets: vec![TargetProgress { target: self.target.clone(), bytes: done, state: TargetState::Writing }],
        }));
    }
}

fn check_cancel(c: &AtomicBool) -> Result<()> {
    if c.load(Ordering::Relaxed) {
        Err(NyError::cancelled())
    } else {
        Ok(())
    }
}

fn io_err(e: std::io::Error, what: &str) -> NyError {
    NyError::from_io(&e, what)
}

fn random_u32() -> u32 {
    let mut b = [0u8; 4];
    let _ = getrandom::fill(&mut b);
    u32::from_le_bytes(b) | 0x0100_0000
}

// ------------------------------------------------------------------------------ the job

/// Format `target`. Destroys everything on it. Refuses anything that is not removable USB/SD media.
pub fn format(target: &Target, opts: &FormatOptions, cancel: Arc<AtomicBool>, emit: &mut dyn FnMut(Event)) -> Result<FormatReport> {
    format_impl(target, opts, cancel, emit, true)
}

/// The same, without the second look at the drive list. Only for tests that stand a loop device in for a USB stick.
#[cfg(test)]
pub(crate) fn format_unchecked_lookup(target: &Target, opts: &FormatOptions, cancel: Arc<AtomicBool>, emit: &mut dyn FnMut(Event)) -> Result<FormatReport> {
    format_impl(target, opts, cancel, emit, false)
}

fn gate(target: &Target, recheck: bool) -> Result<()> {
    if target.kind == TargetKind::File {
        return Ok(()); // an image file is not a drive
    }
    let drive = target
        .drive
        .as_ref()
        .ok_or_else(|| NyError::new(ErrorKind::Unsafe, format!("{} is not a drive NyForge recognises, so it will not be formatted.", target.display)))?;
    if drive.path != target.path.to_string_lossy() {
        return Err(NyError::new(ErrorKind::Unsafe, "The target does not match the drive it was resolved from."));
    }
    check_drive_allowed(drive)?;
    if recheck {
        // Look again, now: the drive list can change between the question and the answer.
        let now = device::list_drives()?;
        let cur = now
            .iter()
            .find(|d| d.path == drive.path)
            .ok_or_else(|| NyError::new(ErrorKind::DeviceGone, format!("{} is no longer connected.", drive.path)))?;
        check_drive_allowed(cur)?;
        if cur.size != drive.size || cur.serial != drive.serial {
            return Err(NyError::new(ErrorKind::Unsafe, format!("{} is not the same drive any more (it was swapped while NyForge was running). Nothing was written.", drive.path)));
        }
    }
    Ok(())
}

fn format_impl(target: &Target, opts: &FormatOptions, cancel: Arc<AtomicBool>, emit: &mut dyn FnMut(Event), recheck: bool) -> Result<FormatReport> {
    // 1. May we touch this at all?
    gate(target, recheck)?;
    let claimed = match target.kind {
        TargetKind::BlockDevice => target.capacity.ok_or_else(|| NyError::other("unknown drive size"))?,
        TargetKind::File => opts
            .file_size
            .or(target.capacity)
            .ok_or_else(|| NyError::invalid("Formatting an image file needs a size.").with_hint("Add --size 4G (for example)."))?,
    };
    if let Some(d) = &target.drive {
        if d.sector_size != 512 {
            return Err(NyError::new(ErrorKind::ImageUnsupported, format!("{} uses {}-byte sectors; NyForge formats 512-byte-sector drives only for now.", d.path, d.sector_size)));
        }
    }

    // 2. Is the request sane? Everything that can be checked without touching the drive is checked now.
    if opts.fit_real_capacity && opts.capacity_test != CapacityTest::Full {
        return Err(NyError::invalid("--fit-real-capacity needs --test-capacity full: only the full test measures the real size exactly."));
    }
    if opts.fit_real_capacity && opts.scheme == Scheme::Gpt {
        return Err(NyError::invalid("--fit-real-capacity cannot be used with GPT (the backup table has to sit at the real end of the drive). Use --scheme mbr."));
    }
    let mut labels = vec![];
    for p in &opts.parts {
        labels.push(fsmake::validate_label(p.fs, &p.label)?);
        if let Some(c) = p.cluster {
            fsmake::validate_cluster(p.fs, c)?;
        }
        if fsmake::needs_tool(p.fs) {
            fsmake::check_tool(p.fs)?;
            if cfg!(windows) && p.fs == Fs::Ntfs && opts.scheme == Scheme::None {
                return Err(NyError::invalid("On Windows an NTFS volume needs a partition table.").with_hint("Use --scheme mbr or --scheme gpt."));
            }
        }
    }
    let mut plan = plan_layout(claimed, opts.scheme, &opts.parts, opts.reserve_bytes)?;
    for p in &plan {
        if let Some(c) = p.spec.cluster {
            fsmake::check_cluster_fits(p.spec.fs, p.bytes(), c)?;
        }
    }

    // 3. Go.
    let started = Instant::now();
    let mut rep = Reporter::new(emit, target.display.clone());
    let summary = opts.parts.iter().map(|p| p.fs.name()).collect::<Vec<_>>().join(" + ");
    (rep.emit)(Event::Start {
        image: format!("a fresh {summary}"),
        container: opts.scheme.name().to_string(),
        total_bytes: Some(claimed),
        targets: vec![target.display.clone()],
    });
    // Windows: its own tools first for a plain quick format. They are what a "format" button there uses, and they
    // work on sticks that drop off the bus when a program writes to the raw disk.
    let mut early_notes: Vec<String> = vec![];
    if cfg!(windows) && target.kind == TargetKind::BlockDevice && opts.engine != Engine::Own {
        if let Some(d) = &target.drive {
            match winnative::eligible(opts, &plan) {
                Ok(mut parts) => {
                    for (np, l) in parts.iter_mut().zip(&labels) {
                        np.label = l.clone();
                    }
                    rep.phase(Phase::Writing, "Formatting with Windows' own tools");
                    match winnative::format_disk(d, opts.scheme, &parts, &cancel) {
                        Ok(()) => {
                            let elapsed = started.elapsed().as_secs_f64();
                            (rep.emit)(Event::TargetFinished { target: target.display.clone(), ok: true, bytes: claimed, error: None });
                            (rep.emit)(Event::Finished { ok: true, bytes: claimed, elapsed_secs: elapsed, avg_speed_bps: 0.0 });
                            return Ok(FormatReport {
                                target: target.display.clone(),
                                scheme: opts.scheme,
                                disk_bytes: claimed,
                                usable_bytes: claimed,
                                partitions: plan
                                    .iter()
                                    .zip(&labels)
                                    .map(|(p, l)| PartitionReport { number: p.number, start_bytes: p.start * SECTOR, size_bytes: p.bytes(), fs: p.spec.fs, label: l.clone() })
                                    .collect(),
                                wipe: opts.wipe,
                                capacity: None,
                                elapsed_secs: elapsed,
                                notes: vec!["Formatted with Windows' own tools.".into()],
                            });
                        }
                        Err(e) if e.kind == ErrorKind::Cancelled => return Err(e),
                        Err(e) if opts.engine == Engine::Windows => return Err(e),
                        Err(e) => {
                            rep.log(format!("Windows' own tools did not manage ({}); trying NyForge's own writer.", e.message));
                            early_notes.push("Windows' own tools could not format this drive, so NyForge's own writer did.".into());
                        }
                    }
                }
                Err(why) if opts.engine == Engine::Windows => {
                    return Err(NyError::invalid(format!("Windows' own tools cannot do this: {why}.")).with_hint("Leave out --engine windows to let NyForge write it itself."));
                }
                Err(_) => {}
            }
        }
    }

    rep.phase(Phase::Preparing, "Releasing the drive");
    let _prep = match &target.drive {
        Some(d) => Some(sys::prepare_device_for_erase(d, opts.unmount)?),
        None => None,
    };
    let file = match target.kind {
        TargetKind::BlockDevice => sys::open_rw_direct(&target.path),
        TargetKind::File => std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(&target.path),
    }
    .map_err(|e| io_err(e, &format!("opening {}", target.display)))?;
    if target.kind == TargetKind::File {
        file.set_len(claimed).map_err(|e| io_err(e, "sizing the image file"))?;
    }
    let align = if target.kind == TargetKind::BlockDevice { 4096 } else { 1 }; // O_DIRECT, NO_BUFFERING and /dev/rdisk all want whole sectors
    let whole = FileIo::new(file, 0, (claimed / SECTOR) * SECTOR, align).recovering(&target.path, target.drive.as_ref().filter(|_| target.kind == TargetKind::BlockDevice), sys::Reopen::RwDirect);
    let mut notes = early_notes;

    let result = (|| -> Result<(Option<CapacityReport>, u64)> {
        // 3a. Is the drive what it says it is?
        let mut cap = None;
        let mut usable_bytes = whole.len();
        if opts.capacity_test != CapacityTest::Off {
            rep.phase(Phase::Writing, "Testing the drive's real capacity");
            rep.restart();
            let mut stage_seen = String::new();
            let passes = if opts.capacity_test == CapacityTest::Full { opts.passes.clamp(1, 4) } else { 1 };
            let mut worst: Option<CapacityReport> = None;
            for pass in 1..=passes {
                if passes > 1 {
                    rep.log(format!("Pass {pass} of {passes}…"));
                }
                let r = {
                    let rep_ref = &mut rep;
                    capacity::run(&whole, opts.capacity_test, prng::random_seed(), &cancel, &mut |stage, done, total| {
                        if stage != stage_seen {
                            stage_seen = stage.to_string();
                            rep_ref.restart();
                            rep_ref.log(if stage == "write" { "Writing test data over the drive…" } else { "Reading it back…" });
                        }
                        rep_ref.progress(done, total);
                    })?
                };
                stage_seen.clear();
                let bad_now = r.bad_blocks;
                let keep = match &worst {
                    None => true,
                    Some(w) => bad_now > w.bad_blocks,
                };
                if keep {
                    worst = Some(r);
                }
            }
            let r = worst.expect("at least one pass");
            if !r.genuine {
                if opts.fit_real_capacity {
                    let real = r.estimated_real & !(MIB - 1);
                    if real < 16 * MIB {
                        return Err(r.to_error());
                    }
                    notes.push(format!("The drive claims {} but only {} works: only that part was formatted.", human_size(r.claimed), human_size(real)));
                    usable_bytes = real;
                    plan = plan_layout(real, opts.scheme, &opts.parts, opts.reserve_bytes)?;
                    for p in &plan {
                        if let Some(c) = p.spec.cluster {
                            fsmake::check_cluster_fits(p.spec.fs, p.bytes(), c)?;
                        }
                    }
                } else {
                    return Err(r.to_error());
                }
            }
            cap = Some(r);
        }
        let usable = whole.sub(0, usable_bytes).map_err(|e| io_err(e, "partitioning"))?;

        // 3b. Wipe.
        let full_test_already_random = opts.capacity_test == CapacityTest::Full && opts.wipe == Wipe::Random;
        if opts.wipe != Wipe::Quick && !full_test_already_random {
            let what = if opts.wipe == Wipe::Zero { "zeros" } else { "random data" };
            rep.phase(Phase::Writing, format!("Overwriting the whole drive with {what}"));
            rep.log(format!("Overwriting the whole drive with {what}…"));
            rep.restart();
            wipe_all(&usable, opts.wipe, &cancel, &mut rep)?;
        }
        // Whatever the mode: old tables and filesystem signatures go (a leftover ext4 superblock inside a new FAT32
        // partition makes `blkid` and file managers disagree about what the drive is).
        rep.phase(Phase::Writing, "Clearing old partition tables and signatures");
        let ulen = usable.len();
        let edge = MIB.min(ulen / 2);
        usable.zero(0, edge).map_err(|e| io_err(e, "clearing the start of the drive"))?;
        usable.zero(ulen - edge, edge).map_err(|e| io_err(e, "clearing the end of the drive"))?;
        for p in &plan {
            let (off, len) = (p.start * SECTOR, p.bytes());
            let e = MIB.min(len / 2);
            usable.zero(off, e).map_err(|e| io_err(e, "clearing a partition"))?;
            usable.zero(off + len - e, e).map_err(|e| io_err(e, "clearing a partition"))?;
        }
        check_cancel(&cancel)?;

        // 3c. Partition table.
        let mut sig = [0u8; 4];
        let _ = getrandom::fill(&mut sig);
        let table: Vec<TablePart> = plan
            .iter()
            .map(|p| TablePart {
                start: p.start,
                sectors: p.sectors,
                mbr_type: p.spec.fs.mbr_type(p.sectors),
                gpt_type: p.spec.fs.gpt_type(),
                unique: tables::random_guid(),
                name: if p.spec.label.is_empty() { p.spec.fs.name().to_string() } else { p.spec.label.clone() },
            })
            .collect();
        let disk_sectors = usable_bytes / SECTOR;
        match opts.scheme {
            Scheme::None => {}
            Scheme::Mbr => {
                rep.phase(Phase::Writing, "Writing the partition table (MBR)");
                let mbr = tables::mbr_sector(u32::from_le_bytes(sig) | 0x0100_0000, &table)?;
                usable.write_at(&mbr, 0).map_err(|e| io_err(e, "writing the partition table"))?;
                if claimed / SECTOR > (1 << 32) && usable_bytes == claimed {
                    notes.push(format!("MBR can only address 2 TiB: the other {} of the drive is unused. Use --scheme gpt to use all of it.", human_size(claimed - (1u64 << 32) * SECTOR)));
                }
            }
            Scheme::Gpt => {
                rep.phase(Phase::Writing, "Writing the partition table (GPT)");
                let g = tables::build_gpt(disk_sectors, tables::random_guid(), &table)?;
                usable.write_at(&g.entries, 2 * SECTOR).map_err(|e| io_err(e, "writing the partition table"))?;
                usable.write_at(&g.entries, g.backup_entries_lba * SECTOR).map_err(|e| io_err(e, "writing the backup partition table"))?;
                usable.write_at(&g.backup_header, g.backup_header_lba * SECTOR).map_err(|e| io_err(e, "writing the backup partition table"))?;
                usable.write_at(&g.primary_header, SECTOR).map_err(|e| io_err(e, "writing the partition table"))?;
                usable.write_at(&g.protective_mbr, 0).map_err(|e| io_err(e, "writing the partition table"))?;
            }
        }
        check_cancel(&cancel)?;

        // 3d. Filesystems we write ourselves.
        for (p, label) in plan.iter().zip(&labels) {
            if fsmake::needs_tool(p.spec.fs) {
                continue;
            }
            rep.phase(Phase::Writing, format!("Creating {} ({})", p.spec.fs.name(), human_size(p.bytes())));
            let region = usable.sub(p.start * SECTOR, p.bytes()).map_err(|e| io_err(e, "partitioning"))?;
            fsmake::make_native(region, &p.spec, label, p.start, random_u32())?;
            check_cancel(&cancel)?;
        }
        rep.phase(Phase::Finalizing, "Flushing to the drive");
        usable.sync().map_err(|e| io_err(e, "flushing the drive"))?;
        Ok((cap, usable_bytes))
    })();
    drop(whole);

    let (capacity_report, usable_bytes) = match result {
        Ok(v) => v,
        Err(mut e) => {
            if e.kind == ErrorKind::Cancelled {
                e = e.with_hint("The drive may be partly erased. Run the format again to make it usable.");
            }
            return Err(e);
        }
    };

    // 3e. ext4 / NTFS: the system tools, after the kernel has seen the new table.
    if plan.iter().any(|p| fsmake::needs_tool(p.spec.fs)) {
        if target.kind == TargetKind::BlockDevice {
            sys::finish_device(&target.path);
        }
        let scratch = target.path.parent().map(|p| p.to_path_buf()).filter(|_| target.kind == TargetKind::File).unwrap_or_else(std::env::temp_dir);
        for (p, label) in plan.iter().zip(&labels) {
            if !fsmake::needs_tool(p.spec.fs) {
                continue;
            }
            check_cancel(&cancel)?;
            rep.phase(Phase::Writing, format!("Creating {} ({}) with the system's mkfs", p.spec.fs.name(), human_size(p.bytes())));
            let job = fsmake::ToolJob { spec: &p.spec, label, start_sector: p.start, bytes: p.bytes(), owner: opts.owner };
            match target.kind {
                TargetKind::BlockDevice if cfg!(windows) => {
                    let path = target.path.to_string_lossy();
                    let disk = device::windows::disk_number(&path).ok_or_else(|| NyError::other(format!("{path} is not a Windows physical drive.")))?;
                    fsmake::make_with_tool(&job, fsmake::ToolTarget::WindowsPartition { disk, number: p.number }, &scratch)?;
                }
                TargetKind::BlockDevice => {
                    let disk = target.path.to_string_lossy().into_owned();
                    let node = if opts.scheme == Scheme::None { target.path.clone() } else { fsmake::partition_node(&disk, p.number) };
                    fsmake::wait_for_node(&node)?;
                    fsmake::make_with_tool(&job, fsmake::ToolTarget::Node(&node), &scratch)?;
                }
                TargetKind::File => {
                    fsmake::make_with_tool(&job, fsmake::ToolTarget::FileRegion { path: &target.path, offset: p.start * SECTOR }, &scratch)?;
                }
            }
        }
        if let Some((u, g)) = opts.owner {
            if plan.iter().any(|p| p.spec.fs.is_ext()) {
                notes.push(format!("The ext volume's top folder belongs to user {u}:{g}, so that user can write to it without root."));
            }
        } else if plan.iter().any(|p| p.spec.fs.is_ext()) {
            notes.push("The ext volume's top folder belongs to root: use sudo or chown it once to write to it as a normal user.".into());
        }
    }
    if target.kind == TargetKind::BlockDevice {
        sys::finish_device(&target.path);
    }
    for p in &plan {
        if p.spec.fs == Fs::Fat32 && p.bytes() > 32 * GIB {
            notes.push("FAT32 above 32 GiB: Windows' own formatter refuses to make it, but Windows, macOS, Linux and most devices read it fine.".into());
            break;
        }
    }

    let elapsed = started.elapsed().as_secs_f64();
    (rep.emit)(Event::TargetFinished { target: target.display.clone(), ok: true, bytes: usable_bytes, error: None });
    (rep.emit)(Event::Finished { ok: true, bytes: usable_bytes, elapsed_secs: elapsed, avg_speed_bps: 0.0 });
    let _ = rep.started;
    Ok(FormatReport {
        target: target.display.clone(),
        scheme: opts.scheme,
        disk_bytes: claimed,
        usable_bytes,
        partitions: plan
            .iter()
            .zip(&labels)
            .map(|(p, l)| PartitionReport { number: p.number, start_bytes: p.start * SECTOR, size_bytes: p.bytes(), fs: p.spec.fs, label: l.clone() })
            .collect(),
        wipe: opts.wipe,
        capacity: capacity_report,
        elapsed_secs: elapsed,
        notes,
    })
}

/// Overwrite all of `io` with zeros or random bytes.
fn wipe_all(io: &FileIo, kind: Wipe, cancel: &AtomicBool, rep: &mut Reporter) -> Result<()> {
    let len = io.len();
    const CHUNK: u64 = 4 * MIB;
    let mut buf = vec![0u8; CHUNK as usize];
    let mut rng = prng::Rng::new(prng::random_seed());
    let mut zeroout_works = kind == Wipe::Zero && cfg!(target_os = "linux");
    let mut off = 0u64;
    while off < len {
        check_cancel(cancel)?;
        let n = (len - off).min(CHUNK);
        let mut done = false;
        if zeroout_works && n % 512 == 0 && off % 512 == 0 {
            // The drive's own "write zeroes" command, when it has one: no data crosses the cable.
            done = sys::zero_range(&io.file(), off, n);
            zeroout_works = done;
        }
        if !done {
            if kind == Wipe::Random {
                rng.fill(&mut buf[..n as usize]);
            }
            io.write_at(&buf[..n as usize], off).map_err(|e| io_err(e, "overwriting the drive"))?;
        }
        off += n;
        rep.progress(off, len);
    }
    io.sync().map_err(|e| io_err(e, "flushing the drive"))?;
    Ok(())
}
