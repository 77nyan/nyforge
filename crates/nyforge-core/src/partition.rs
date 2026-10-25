//! Read a disk's partition table (MBR or GPT) through any [`Io`].
//! Read-only; 512-byte sectors.

use crate::error::{NyError, Result};
use crate::nyboot::io::Io;

pub const SECTOR: u64 = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartKind {
    Mbr(u8),
    Gpt { type_guid: [u8; 16], name: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    /// 1-based, as Linux numbers them.
    pub index: u32,
    pub start: u64,
    pub sectors: u64,
    pub kind: PartKind,
}

impl Partition {
    pub fn bytes(&self) -> u64 {
        self.sectors * SECTOR
    }
}

fn io_err(e: std::io::Error) -> NyError {
    NyError::from_io(&e, "reading the partition table")
}

pub fn read_table(io: &dyn Io) -> Result<Vec<Partition>> {
    let mut s0 = [0u8; 512];
    io.read_at(&mut s0, 0).map_err(io_err)?;
    if s0[510] != 0x55 || s0[511] != 0xAA {
        return Ok(vec![]);
    }
    let mut mbr = vec![];
    let mut protective = false;
    for i in 0..4usize {
        let e = &s0[446 + i * 16..462 + i * 16];
        let ty = e[4];
        let start = u32::from_le_bytes(e[8..12].try_into().unwrap()) as u64;
        let len = u32::from_le_bytes(e[12..16].try_into().unwrap()) as u64;
        if ty == 0 || len == 0 {
            continue;
        }
        if ty == 0xEE {
            protective = true;
        }
        mbr.push(Partition { index: i as u32 + 1, start, sectors: len, kind: PartKind::Mbr(ty) });
    }
    if protective {
        if let Some(g) = read_gpt(io)? {
            return Ok(g);
        }
    }
    Ok(mbr.into_iter().filter(|p| !matches!(p.kind, PartKind::Mbr(0xEE))).collect())
}

fn read_gpt(io: &dyn Io) -> Result<Option<Vec<Partition>>> {
    let mut h = [0u8; 512];
    io.read_at(&mut h, SECTOR).map_err(io_err)?;
    if &h[0..8] != b"EFI PART" {
        return Ok(None);
    }
    let entries_lba = u64::from_le_bytes(h[72..80].try_into().unwrap());
    let count = u32::from_le_bytes(h[80..84].try_into().unwrap()) as u64;
    let esize = u32::from_le_bytes(h[84..88].try_into().unwrap()) as u64;
    if esize < 128 || esize > 4096 || count == 0 || count > 1024 {
        return Err(NyError::invalid("The GPT partition table is damaged."));
    }
    let total = count * esize;
    if entries_lba * SECTOR + total > io.len() {
        return Err(NyError::invalid("The GPT partition table is damaged."));
    }
    let mut buf = vec![0u8; total as usize];
    io.read_at(&mut buf, entries_lba * SECTOR).map_err(io_err)?;
    let mut out = vec![];
    for i in 0..count as usize {
        let e = &buf[i * esize as usize..(i + 1) * esize as usize];
        if e[0..16].iter().all(|&b| b == 0) {
            continue;
        }
        let first = u64::from_le_bytes(e[32..40].try_into().unwrap());
        let last = u64::from_le_bytes(e[40..48].try_into().unwrap());
        if last < first || (last + 1) * SECTOR > io.len() {
            continue;
        }
        let u: Vec<u16> = e[56..128].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&c| c != 0).collect();
        let mut guid = [0u8; 16];
        guid.copy_from_slice(&e[0..16]);
        out.push(Partition { index: i as u32 + 1, start: first, sectors: last - first + 1, kind: PartKind::Gpt { type_guid: guid, name: String::from_utf16_lossy(&u) } });
    }
    Ok(Some(out))
}

/// Does this partition start with a FAT boot sector?
pub fn looks_like_fat(io: &dyn Io, p: &Partition) -> bool {
    let mut b = [0u8; 512];
    if io.read_at(&mut b, p.start * SECTOR).is_err() {
        return false;
    }
    b[510] == 0x55 && b[511] == 0xAA && (&b[0x52..0x57] == b"FAT32" || &b[0x36..0x39] == b"FAT")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nyboot::io::FileIo;
    use std::process::Command;

    fn have(c: &str) -> bool {
        Command::new("sh").args(["-c", &format!("command -v {c} >/dev/null")]).status().map(|s| s.success()).unwrap_or(false)
    }

    /// These tests need external tools that only Linux machines have. On Linux a missing tool is an error (CI installs
    /// them); on macOS and Windows the test is skipped, because those systems cannot run it at all.
    fn skip_without(tools: &[&str]) -> bool {
        let missing: Vec<&&str> = tools.iter().filter(|t| !have(t)).collect();
        if missing.is_empty() {
            return false;
        }
        assert!(!cfg!(target_os = "linux"), "{missing:?} required for this test");
        eprintln!("skipped: {missing:?} not available on this system");
        true
    }

    /// Partition tables are made by `parted`, an independent implementation.
    fn parted_image(name: &str, label: &str) -> std::path::PathBuf {
        assert!(have("parted"), "parted is required for this test");
        let p = std::env::temp_dir().join(format!("nyforge-part-{}-{}.img", std::process::id(), name));
        let f = std::fs::File::create(&p).unwrap();
        f.set_len(64 << 20).unwrap();
        drop(f);
        let o = Command::new("parted")
            .args(["-s"])
            .arg(&p)
            .args(["mklabel", label, "mkpart", if label == "gpt" { "p1" } else { "primary" }, "fat32", "2048s", "67583s", "mkpart", if label == "gpt" { "p2" } else { "primary" }, "ext4", "69632s", "102399s"])
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        p
    }

    fn open(p: &std::path::Path) -> FileIo {
        let f = std::fs::OpenOptions::new().read(true).write(true).open(p).unwrap();
        let len = f.metadata().unwrap().len();
        FileIo::new(f, 0, len, 1)
    }

    #[test]
    fn reads_mbr_tables() {
        if skip_without(&["parted"]) {
            return;
        }
        let p = parted_image("mbr", "msdos");
        let t = read_table(&open(&p)).unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!((t[0].index, t[0].start, t[0].sectors), (1, 2048, 65536));
        assert!(matches!(t[0].kind, PartKind::Mbr(0x0b | 0x0c)), "{:?}", t[0].kind);
        assert_eq!((t[1].start, t[1].sectors, t[1].kind.clone()), (69632, 32768, PartKind::Mbr(0x83)));
    }

    #[test]
    fn reads_gpt_tables() {
        if skip_without(&["parted"]) {
            return;
        }
        let p = parted_image("gpt", "gpt");
        let t = read_table(&open(&p)).unwrap();
        assert_eq!(t.len(), 2, "{t:?}");
        assert_eq!((t[0].start, t[0].sectors), (2048, 65536));
        assert!(matches!(&t[0].kind, PartKind::Gpt { name, .. } if name == "p1"), "{:?}", t[0].kind);
        assert_eq!((t[1].start, t[1].sectors), (69632, 32768));
    }

    #[test]
    fn blank_disk_has_no_partitions() {
        let p = std::env::temp_dir().join(format!("nyforge-part-blank-{}.img", std::process::id()));
        std::fs::write(&p, vec![0u8; 1 << 20]).unwrap();
        assert!(read_table(&open(&p)).unwrap().is_empty());
    }
}
