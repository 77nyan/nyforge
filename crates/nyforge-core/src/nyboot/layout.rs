//! Disk layout and the MBR/boot-code bytes.
//!
//! ```text
//! LBA 0        MBR: GRUB boot.img + partition table
//! LBA 1..      GRUB core.img (self-contained, lives in the gap before partition 1)
//! partition 1  NYBOOT       exFAT, your ISOs (first, so Windows/macOS/Linux all show it)
//! partition 2  NYBOOT-EFI   FAT32, UEFI GRUB + menu scripts (at the end of the disk)
//! ```

use super::assets;
use crate::error::{NyError, Result};

pub const SECTOR: u64 = 512;
pub const MIB_SECTORS: u64 = 2048;
pub const MAX_MBR_SECTORS: u64 = 0xFFFF_FFFF;
pub const MIN_DISK_BYTES: u64 = 512 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub disk_sectors: u64,
    pub data_start: u64,
    pub data_sectors: u64,
    pub esp_start: u64,
    pub esp_sectors: u64,
    /// Space left unpartitioned after the ESP, for the user (in sectors).
    pub reserved_sectors: u64,
}

impl Layout {
    pub fn plan(disk_bytes: u64, esp_mib: u64, reserve_mib: u64) -> Result<Layout> {
        if disk_bytes < MIN_DISK_BYTES {
            return Err(NyError::invalid(format!(
                "That drive is too small for NyBoot ({} needed at least).",
                crate::format::human_size(MIN_DISK_BYTES)
            )));
        }
        if !(32..=1024).contains(&esp_mib) {
            return Err(NyError::invalid("The boot partition must be between 32 and 1024 MiB."));
        }
        let usable = (disk_bytes / SECTOR).min(MAX_MBR_SECTORS) & !(MIB_SECTORS - 1);
        let reserved = reserve_mib * MIB_SECTORS;
        let esp_sectors = esp_mib * MIB_SECTORS;
        if usable < MIB_SECTORS + esp_sectors + reserved + 256 * MIB_SECTORS {
            return Err(NyError::invalid("That drive is too small for the requested sizes."));
        }
        let esp_start = usable - reserved - esp_sectors;
        Ok(Layout {
            disk_sectors: disk_bytes / SECTOR,
            data_start: MIB_SECTORS,
            data_sectors: esp_start - MIB_SECTORS,
            esp_start,
            esp_sectors,
            reserved_sectors: reserved,
        })
    }

    pub fn data_bytes(&self) -> u64 {
        self.data_sectors * SECTOR
    }
}

/// The 512-byte MBR: GRUB's boot.img with our disk signature and partition table.
pub fn build_mbr(layout: &Layout, disk_signature: u32) -> [u8; 512] {
    let mut m = [0u8; 512];
    m.copy_from_slice(&assets::BOOT_IMG[..512]);
    // Hard disk, not floppy: skip boot.img's drive-number sanity jump (what grub-bios-setup does).
    m[0x66] = 0x90;
    m[0x67] = 0x90;
    // boot.img points at core.img's first sector: LBA 1 (already the default value, set explicitly).
    m[0x5C..0x64].copy_from_slice(&1u64.to_le_bytes());
    // Everything after the boot code belongs to the partition table. GRUB's boot.img is NOT
    // zero there (it carries a few bytes of its own), and GRUB's partition parser rejects
    // garbage "boot flags" in unused entries, so clear it all first.
    m[440..510].fill(0);
    m[440..444].copy_from_slice(&disk_signature.to_le_bytes());
    let entry = |active: bool, ty: u8, start: u64, len: u64| -> [u8; 16] {
        let mut e = [0u8; 16];
        e[0] = if active { 0x80 } else { 0 };
        e[1..4].copy_from_slice(&[0xFE, 0xFF, 0xFF]);
        e[4] = ty;
        e[5..8].copy_from_slice(&[0xFE, 0xFF, 0xFF]);
        e[8..12].copy_from_slice(&(start as u32).to_le_bytes());
        e[12..16].copy_from_slice(&(len as u32).to_le_bytes());
        e
    };
    m[446..462].copy_from_slice(&entry(true, 0x07, layout.data_start, layout.data_sectors));
    m[462..478].copy_from_slice(&entry(false, 0xEF, layout.esp_start, layout.esp_sectors));
    m[510] = 0x55;
    m[511] = 0xAA;
    m
}

/// Parse an MBR's four partition entries: `(type, start_lba, sectors)`, zero-length ones skipped.
pub fn parse_mbr(sector0: &[u8]) -> Option<Vec<(u8, u64, u64)>> {
    if sector0.len() < 512 || sector0[510] != 0x55 || sector0[511] != 0xAA {
        return None;
    }
    let mut v = vec![];
    for i in 0..4 {
        let e = &sector0[446 + i * 16..462 + i * 16];
        let start = u32::from_le_bytes(e[8..12].try_into().unwrap()) as u64;
        let len = u32::from_le_bytes(e[12..16].try_into().unwrap()) as u64;
        if e[4] != 0 && len > 0 {
            v.push((e[4], start, len));
        }
    }
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_is_aligned_and_fits() {
        for gib in [1u64, 4, 16, 64, 512, 4096] {
            let l = Layout::plan(gib << 30, 64, 0).unwrap();
            assert_eq!(l.data_start % MIB_SECTORS, 0);
            assert_eq!(l.esp_start % MIB_SECTORS, 0);
            assert_eq!(l.data_start + l.data_sectors, l.esp_start);
            assert!(l.esp_start + l.esp_sectors <= MAX_MBR_SECTORS + 1);
            assert!(l.esp_start + l.esp_sectors <= l.disk_sectors);
        }
    }

    #[test]
    fn reserved_space_is_left_free() {
        let l = Layout::plan(8 << 30, 64, 1024).unwrap();
        assert_eq!(l.disk_sectors - (l.esp_start + l.esp_sectors), 1024 * MIB_SECTORS);
    }

    #[test]
    fn tiny_drives_are_refused() {
        assert!(Layout::plan(256 << 20, 64, 0).is_err());
        assert!(Layout::plan(8 << 30, 8, 0).is_err());
        assert!(Layout::plan(600 << 20, 64, 400).is_err());
    }

    #[test]
    fn mbr_roundtrip() {
        let l = Layout::plan(16 << 30, 64, 0).unwrap();
        let m = build_mbr(&l, 0xCAFE_F00D);
        let p = parse_mbr(&m).unwrap();
        assert_eq!(p, vec![(0x07, l.data_start, l.data_sectors), (0xEF, l.esp_start, l.esp_sectors)]);
        assert_eq!(&m[440..444], &0xCAFE_F00Du32.to_le_bytes());
        assert_eq!(m[0x66..0x68], [0x90, 0x90]);
        assert_eq!(m[446], 0x80, "data partition is active (some BIOSes insist on one)");
        assert!(m[478..510].iter().all(|&b| b == 0), "unused entries must be all zero");
        for e in 0..4 {
            let flag = m[446 + e * 16];
            assert!(flag == 0 || flag == 0x80, "entry {e} has a bad boot flag {flag:#x}");
        }
    }
}
