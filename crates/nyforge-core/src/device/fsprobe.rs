//! Identify a filesystem by looking at its first bytes.  No external tools,
//! no udev: works the same on every platform that can read the device.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FsInfo {
    pub fstype: Option<String>,
    pub label: Option<String>,
}

fn trim_label(b: &[u8]) -> Option<String> {
    let s: String = String::from_utf8_lossy(b).trim_matches(|c: char| c == '\0' || c.is_whitespace()).to_string();
    if s.is_empty() || s == "NO NAME" {
        None
    } else {
        Some(s)
    }
}

/// Classify a buffer holding the first ~64 KiB of a volume (plus, for some
/// filesystems, data further in that we read separately).
pub fn probe_bytes(head: &[u8], tail_at_64k: &[u8]) -> FsInfo {
    let g = |off: usize, len: usize| -> &[u8] {
        if off + len <= head.len() {
            &head[off..off + len]
        } else {
            &[]
        }
    };

    if g(3, 8) == b"EXFAT   " {
        return FsInfo { fstype: Some("exfat".into()), label: None };
    }
    if g(3, 4) == b"NTFS" {
        return FsInfo { fstype: Some("ntfs".into()), label: None };
    }
    if g(0x52, 5) == b"FAT32" {
        return FsInfo { fstype: Some("vfat".into()), label: trim_label(g(0x47, 11)) };
    }
    if g(0x36, 3) == b"FAT" {
        return FsInfo { fstype: Some("vfat".into()), label: trim_label(g(0x2b, 11)) };
    }
    // ISO 9660: "CD001" at 0x8001, volume id at 0x8028
    if g(0x8001, 5) == b"CD001" {
        return FsInfo { fstype: Some("iso9660".into()), label: trim_label(g(0x8028, 32)) };
    }
    // ext2/3/4: magic 0xEF53 at 1080, label at 1144
    if g(1080, 2) == [0x53, 0xEF] {
        return FsInfo { fstype: Some("ext4".into()), label: trim_label(g(1144, 16)) };
    }
    if g(0, 4) == b"LUKS" {
        return FsInfo { fstype: Some("crypto_LUKS".into()), label: None };
    }
    if g(0, 4) == b"XFSB" {
        return FsInfo { fstype: Some("xfs".into()), label: trim_label(g(108, 12)) };
    }
    if g(0x400, 4) == [0x10, 0x20, 0xF5, 0xF2] {
        return FsInfo { fstype: Some("f2fs".into()), label: None };
    }
    if tail_at_64k.len() >= 0x48 && &tail_at_64k[0x40..0x48] == b"_BHRfS_M" {
        return FsInfo { fstype: Some("btrfs".into()), label: trim_label(&tail_at_64k[0x12b..0x12b + 0x100.min(tail_at_64k.len().saturating_sub(0x12b))]) };
    }
    if g(4086, 10) == b"SWAPSPACE2" || g(4086, 10) == b"SWAP-SPACE" {
        return FsInfo { fstype: Some("swap".into()), label: None };
    }
    FsInfo::default()
}

/// Probe a block device or image file. Returns empty info if unreadable.
pub fn probe_path(path: &Path) -> FsInfo {
    let Ok(mut f) = File::open(path) else { return FsInfo::default() };
    let mut head = vec![0u8; 0x8200];
    let n = f.read(&mut head).unwrap_or(0);
    head.truncate(n);
    let mut tail = vec![0u8; 0x400];
    let tail_n = if f.seek(SeekFrom::Start(0x10000)).is_ok() { f.read(&mut tail).unwrap_or(0) } else { 0 };
    tail.truncate(tail_n);
    probe_bytes(&head, &tail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_fat32_with_label() {
        let mut b = vec![0u8; 0x8200];
        b[0x52..0x57].copy_from_slice(b"FAT32");
        b[0x47..0x47 + 11].copy_from_slice(b"MYSTICK    ");
        let i = probe_bytes(&b, &[]);
        assert_eq!(i.fstype.as_deref(), Some("vfat"));
        assert_eq!(i.label.as_deref(), Some("MYSTICK"));
    }

    #[test]
    fn recognises_ext_and_iso() {
        let mut b = vec![0u8; 0x8200];
        b[1080..1082].copy_from_slice(&[0x53, 0xEF]);
        b[1144..1148].copy_from_slice(b"root");
        assert_eq!(probe_bytes(&b, &[]).label.as_deref(), Some("root"));

        let mut c = vec![0u8; 0x8200];
        c[0x8001..0x8006].copy_from_slice(b"CD001");
        c[0x8028..0x8028 + 8].copy_from_slice(b"UBUNTU24");
        let i = probe_bytes(&c, &[]);
        assert_eq!(i.fstype.as_deref(), Some("iso9660"));
        assert_eq!(i.label.as_deref(), Some("UBUNTU24"));
    }

    #[test]
    fn blank_is_unknown() {
        assert_eq!(probe_bytes(&vec![0u8; 0x8200], &[]), FsInfo::default());
    }
}
