//! Drives: what is plugged in, what is safe to overwrite, and why.
//!
//! The rule that matters most: **never offer a drive that holds the running
//! system**, and never let one be selected even if someone asks very nicely.

pub mod fsprobe;
#[cfg(target_os = "linux")]
pub mod linux;
pub mod macos;
pub mod windows;

use crate::error::{ErrorKind, NyError, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Bus {
    Usb,
    Sd,
    Mmc,
    Nvme,
    Sata,
    Virtual,
    Unknown,
}

impl Bus {
    pub fn label(self) -> &'static str {
        match self {
            Bus::Usb => "USB",
            Bus::Sd => "SD card",
            Bus::Mmc => "eMMC",
            Bus::Nvme => "NVMe",
            Bus::Sata => "SATA",
            Bus::Virtual => "Virtual",
            Bus::Unknown => "Drive",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Safety {
    /// Removable media: the normal case.
    Safe,
    /// A fixed, non-system disk. Needs an explicit "I mean it".
    Internal,
    /// Loop/virtual device. Only usable when named explicitly.
    Virtual,
    /// Holds the running OS (or swap). Never writable.
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Partition {
    pub path: String,
    pub number: u32,
    pub size: u64,
    pub fstype: Option<String>,
    pub label: Option<String>,
    pub mountpoints: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Drive {
    /// `/dev/sdb`, `\\.\PhysicalDrive2`, ...
    pub path: String,
    pub vendor: String,
    pub model: String,
    pub serial: Option<String>,
    pub size: u64,
    pub sector_size: u32,
    pub bus: Bus,
    pub removable: bool,
    pub read_only: bool,
    pub safety: Safety,
    pub partitions: Vec<Partition>,
}

impl Drive {
    /// "SanDisk Ultra" or "USB drive" if the firmware says nothing useful.
    pub fn display_name(&self) -> String {
        let vendor = self.vendor.trim();
        // Virtio and friends report a bare PCI id like "0x1af4": not a name.
        let vendor = if vendor.starts_with("0x") { "" } else { vendor };
        let n = format!("{} {}", vendor, self.model.trim()).trim().to_string();
        if n.is_empty() {
            format!("{} drive", self.bus.label())
        } else {
            n
        }
    }

    pub fn mounted_points(&self) -> Vec<&str> {
        self.partitions.iter().flat_map(|p| p.mountpoints.iter().map(|s| s.as_str())).collect()
    }

    /// True when a partition carries a NyBoot label (`NYBOOT` data or `NYBOOT-EFI` boot partition). The label
    /// is only a hint; reading the drive (`nyforge boot ls`) is what proves it.
    pub fn is_nyboot(&self) -> bool {
        self.partitions.iter().any(|p| {
            p.label.as_deref().map(|l| l.eq_ignore_ascii_case("NYBOOT") || l.eq_ignore_ascii_case("NYBOOT-EFI")).unwrap_or(false)
        })
    }

    /// Short human summary of what is on it, for the "this will erase" prompt.
    pub fn contents_summary(&self) -> String {
        if self.partitions.is_empty() {
            return "no partitions".into();
        }
        let parts: Vec<String> = self
            .partitions
            .iter()
            .map(|p| match (&p.label, &p.fstype) {
                (Some(l), Some(f)) if !l.is_empty() => format!("\"{l}\" ({f})"),
                (None, Some(f)) | (Some(_), Some(f)) => format!("{f} partition"),
                _ => "unrecognised partition".into(),
            })
            .collect();
        parts.join(", ")
    }
}

/// Everything attached, classified. Nothing is hidden here: callers decide
/// what to show using [`Drive::safety`].
pub fn list_drives() -> Result<Vec<Drive>> {
    #[cfg(target_os = "linux")]
    {
        linux::list_drives()
    }
    #[cfg(target_os = "macos")]
    {
        macos::list_drives()
    }
    #[cfg(windows)]
    {
        windows::list_drives()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        Err(NyError::new(ErrorKind::Unavailable, "Drive detection is not available on this platform."))
    }
}

/// Drives to show by default: removable media only.
pub fn list_safe_drives() -> Result<Vec<Drive>> {
    Ok(list_drives()?.into_iter().filter(|d| d.safety == Safety::Safe && d.size > 0).collect())
}

/// What a write target turned out to be.
#[derive(Debug, Clone)]
pub enum ResolvedTarget {
    Device(Drive),
    /// A plain file (writing an image file instead of a drive).
    File { path: String },
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TargetPolicy {
    /// Permit non-system fixed disks.
    pub allow_internal: bool,
    /// Permit loop/virtual devices (tests, VMs).
    pub allow_virtual: bool,
}

/// The last line of defence before bytes hit a disk.
pub fn resolve_target(spec: &str, policy: TargetPolicy) -> Result<ResolvedTarget> {
    let normalized = normalize_spec(spec);
    let spec = normalized.as_str();
    let p = Path::new(spec);
    if let Ok(md) = std::fs::metadata(p) {
        if md.is_file() {
            return Ok(ResolvedTarget::File { path: spec.to_string() });
        }
    } else if !spec.starts_with("/dev/") && !spec.starts_with("\\\\.\\") {
        // A path that does not exist and is not a device: treat as a new image file.
        return Ok(ResolvedTarget::File { path: spec.to_string() });
    }

    let drives = list_drives()?;
    let canon = canonical_device_path(spec);
    if let Some(d) = drives.iter().find(|d| same_device(&d.path, &canon)) {
        return check_drive(d.clone(), policy);
    }
    // Maybe they named a partition.
    for d in &drives {
        if let Some(part) = d.partitions.iter().find(|pt| same_device(&pt.path, &canon)) {
            return Err(NyError::new(
                ErrorKind::InvalidInput,
                format!("{} is a partition, not a whole drive.", part.path),
            )
            .with_hint(format!("Flash to the whole drive instead: {}", d.path)));
        }
    }
    Err(NyError::new(ErrorKind::InvalidInput, format!("{spec} is not a drive NyForge can see."))
        .with_hint("Run `nyforge list` to see available drives."))
}

fn check_drive(d: Drive, policy: TargetPolicy) -> Result<ResolvedTarget> {
    match d.safety {
        Safety::System => Err(NyError::new(
            ErrorKind::Unsafe,
            format!("{} ({}) holds the running system. NyForge will never write to it.", d.path, d.display_name()),
        )),
        Safety::Internal if !policy.allow_internal => Err(NyError::new(
            ErrorKind::Unsafe,
            format!("{} ({}, {}) is a fixed internal disk, not removable media.", d.path, d.display_name(), crate::format::human_size(d.size)),
        )
        .with_hint("If you really mean to overwrite it, repeat the command with --allow-internal.")),
        Safety::Virtual if !policy.allow_virtual => Err(NyError::new(
            ErrorKind::Unsafe,
            format!("{} is a virtual/loop device.", d.path),
        )
        .with_hint("Repeat the command with --allow-virtual if that is what you want.")),
        _ => {
            if d.read_only {
                return Err(NyError::new(ErrorKind::ReadOnly, format!("{} is marked read-only.", d.path))
                    .with_hint("Check for a write-protect switch on the drive or SD adapter."));
            }
            Ok(ResolvedTarget::Device(d))
        }
    }
}

fn canonical_device_path(spec: &str) -> String {
    let spec = normalize_spec(spec);
    std::fs::canonicalize(&spec).map(|p| p.to_string_lossy().into_owned()).unwrap_or(spec)
}

/// Let people name a drive the way their platform's tools do: macOS's raw `/dev/rdisk4` is the same
/// drive as `/dev/disk4`; Windows' `PhysicalDrive2` is `\\.\PhysicalDrive2`.
pub fn normalize_spec(spec: &str) -> String {
    if cfg!(target_os = "macos") {
        return crate::sys::mac_block_path(spec);
    }
    if cfg!(windows) {
        return windows::normalize_spec(spec);
    }
    spec.to_string()
}

fn same_device(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nyboot_labels_are_recognised() {
        let part = |l: &str| Partition { path: "/dev/sdb1".into(), number: 1, size: 1, fstype: None, label: Some(l.into()), mountpoints: vec![] };
        let mut d = Drive { path: "/dev/sdb".into(), vendor: "".into(), model: "".into(), serial: None, size: 1, sector_size: 512, bus: Bus::Usb, removable: true, read_only: false, safety: Safety::Safe, partitions: vec![part("DATA")] };
        assert!(!d.is_nyboot());
        d.partitions.push(part("nyboot"));
        assert!(d.is_nyboot());
        d.partitions = vec![part("NYBOOT-EFI")];
        assert!(d.is_nyboot());
    }

    #[test]
    fn display_name_falls_back() {
        let d = Drive {
            path: "/dev/sdb".into(),
            vendor: " ".into(),
            model: "".into(),
            serial: None,
            size: 8_000_000_000,
            sector_size: 512,
            bus: Bus::Usb,
            removable: true,
            read_only: false,
            safety: Safety::Safe,
            partitions: vec![],
        };
        assert_eq!(d.display_name(), "USB drive");
    }

    #[test]
    fn nonexistent_plain_path_is_an_image_file() {
        let r = resolve_target("/tmp/nyforge-new-output.img", TargetPolicy::default()).unwrap();
        assert!(matches!(r, ResolvedTarget::File { .. }));
    }
}

#[cfg(test)]
mod name_tests {
    use super::*;

    #[test]
    fn pci_ids_are_not_vendor_names() {
        let d = Drive {
            path: "/dev/vda".into(),
            vendor: "0x1af4".into(),
            model: "".into(),
            serial: None,
            size: 1,
            sector_size: 512,
            bus: Bus::Virtual,
            removable: false,
            read_only: false,
            safety: Safety::System,
            partitions: vec![],
        };
        assert_eq!(d.display_name(), "Virtual drive");
    }
}
