//! A small, careful exFAT implementation: format a partition, then list, add
//! and remove files and directories **without mounting** (so it behaves the
//! same on Linux, Windows and macOS, and works on plain image files in tests).
//!
//! Scope, on purpose:
//! * 512-byte logical sectors.
//! * Files are stored contiguously (flag `NoFatChain`), which also gives us the
//!   exact on-disk extent of every ISO for later use.
//! * Directories always use a FAT chain. The root directory can grow; other
//!   directories live in the one cluster they are created with.
//! * No fragmentation handling when adding: if there is no free run big
//!   enough the add fails with a clear message.

use super::io::Io;
use crate::error::{ErrorKind, NyError, Result};
use std::io::Read;

pub const SECTOR: u64 = 512;
const UPCASE_STD: &[u8] = include_bytes!("../../../../assets/exfat_upcase.bin");

const EOC: u32 = 0xFFFF_FFFF;
const ATTR_DIR: u16 = 0x10;
const ATTR_ARCHIVE: u16 = 0x20;

#[derive(Debug, Clone)]
pub struct FormatParams {
    /// Up to 11 characters.
    pub label: String,
    pub serial: u32,
    /// Override the cluster size in bytes (power of two, 512..=32 MiB).
    pub cluster_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub first_cluster: u32,
    pub size: u64,
    pub valid_size: u64,
    pub no_fat_chain: bool,
    /// Where the entry set lives: (cluster-aligned byte index inside the directory, entry count).
    loc: (usize, usize),
}

#[derive(Debug, Clone, Copy)]
struct DirRef {
    first: u32,
    /// Bytes, for directories with an explicit length; root derives it from its chain.
    len: u64,
    no_fat_chain: bool,
    is_root: bool,
}

pub struct ExFat<I: Io> {
    io: I,
    cluster_bytes: u64,
    fat_off: u64,
    heap_off: u64,
    cluster_count: u32,
    root: u32,
    bitmap: Vec<u8>,
    bitmap_first: u32,
    bitmap_dirty: bool,
    upcase: Vec<u16>,
    alloc_hint: u32,
}

// ---------------------------------------------------------------- checksums

fn checksum32(data: &[u8], skip: impl Fn(usize) -> bool) -> u32 {
    let mut c: u32 = 0;
    for (i, &b) in data.iter().enumerate() {
        if skip(i) {
            continue;
        }
        c = (if c & 1 != 0 { 0x8000_0000u32 } else { 0u32 }).wrapping_add(c >> 1).wrapping_add(b as u32);
    }
    c
}

fn set_checksum(set: &[u8]) -> u16 {
    let mut c: u16 = 0;
    for (i, &b) in set.iter().enumerate() {
        if i == 2 || i == 3 {
            continue;
        }
        c = (if c & 1 != 0 { 0x8000u16 } else { 0u16 }).wrapping_add(c >> 1).wrapping_add(b as u16);
    }
    c
}

fn decode_upcase(table: &[u8]) -> Vec<u16> {
    let words: Vec<u16> = table.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let mut out: Vec<u16> = Vec::with_capacity(65536);
    let mut i = 0;
    while i < words.len() && out.len() < 65536 {
        if words[i] == 0xFFFF && i + 1 < words.len() {
            let n = words[i + 1] as usize;
            for _ in 0..n {
                let c = out.len() as u16;
                out.push(c);
            }
            i += 2;
        } else {
            out.push(words[i]);
            i += 1;
        }
    }
    out
}

fn dos_timestamp() -> u32 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    let days = secs.div_euclid(86400);
    let tod = secs.rem_euclid(86400);
    // civil-from-days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let year = (y.clamp(1980, 2107) - 1980) as u32;
    (year << 25) | ((m as u32) << 21) | ((d as u32) << 16) | (((tod / 3600) as u32) << 11) | ((((tod / 60) % 60) as u32) << 5) | ((tod % 60 / 2) as u32)
}

fn invalid_name(name: &str) -> Option<String> {
    if name.is_empty() || name == "." || name == ".." {
        return Some(format!("\"{name}\" is not a valid file name."));
    }
    if name.encode_utf16().count() > 255 {
        return Some("The file name is longer than 255 characters.".into());
    }
    if let Some(c) = name.chars().find(|c| (*c as u32) < 0x20 || "\"*/:<>?\\|".contains(*c)) {
        return Some(format!("The file name contains a character exFAT does not allow ({c:?})."));
    }
    if name.ends_with(' ') || name.ends_with('.') {
        return Some("File names cannot end with a space or a dot.".into());
    }
    None
}

fn corrupt(msg: impl Into<String>) -> NyError {
    NyError::new(ErrorKind::Other, format!("The exFAT filesystem looks damaged: {}", msg.into()))
}

fn io_err(e: std::io::Error, what: &str) -> NyError {
    NyError::from_io(&e, what)
}

impl<I: Io> ExFat<I> {
    // ------------------------------------------------------------ format

    pub fn pick_cluster_bytes(volume_bytes: u64) -> u64 {
        if volume_bytes <= 256 << 20 {
            4096
        } else if volume_bytes <= 32 << 30 {
            32 << 10
        } else {
            128 << 10
        }
    }

    /// Create an empty filesystem covering all of `io`.
    pub fn format(io: I, p: &FormatParams) -> Result<Self> {
        let total = io.len() / SECTOR;
        if total < 4096 {
            return Err(NyError::invalid("That partition is too small for exFAT."));
        }
        let cluster_bytes = p.cluster_bytes.unwrap_or_else(|| Self::pick_cluster_bytes(io.len()));
        if !cluster_bytes.is_power_of_two() || !(512..=(32 << 20)).contains(&cluster_bytes) {
            return Err(NyError::invalid("Invalid exFAT cluster size."));
        }
        let spc = cluster_bytes / SECTOR;
        let label: Vec<u16> = p.label.encode_utf16().collect();
        if label.len() > 11 {
            return Err(NyError::invalid("An exFAT label can be at most 11 characters."));
        }

        let fat_off = 2048u64.min(total / 4).max(24).next_multiple_of(8);
        // Size the FAT for the worst case, then place the heap right after it.
        let approx_clusters = (total - fat_off) / spc + 1;
        let fat_len = ((approx_clusters + 2) * 4).div_ceil(SECTOR);
        let heap_off = (fat_off + fat_len).next_multiple_of(spc.max(8));
        if heap_off >= total {
            return Err(NyError::invalid("That partition is too small for exFAT."));
        }
        let cluster_count = ((total - heap_off) / spc).min(0xFFFF_FFF5 - 2) as u32;
        if cluster_count < 16 {
            return Err(NyError::invalid("That partition is too small for exFAT."));
        }

        let bitmap_bytes = (cluster_count as u64).div_ceil(8);
        let bitmap_clusters = bitmap_bytes.div_ceil(cluster_bytes) as u32;
        let upcase_clusters = (UPCASE_STD.len() as u64).div_ceil(cluster_bytes) as u32;
        let bitmap_first = 2u32;
        let upcase_first = bitmap_first + bitmap_clusters;
        let root = upcase_first + upcase_clusters;

        let mut fs = ExFat {
            io,
            cluster_bytes,
            fat_off: fat_off * SECTOR,
            heap_off: heap_off * SECTOR,
            cluster_count,
            root,
            bitmap: vec![0u8; bitmap_bytes as usize],
            bitmap_first,
            bitmap_dirty: true,
            upcase: decode_upcase(UPCASE_STD),
            alloc_hint: 2,
        };

        // Wipe everything before the heap (old signatures!) and the tail, so no
        // stale filesystem can be mistaken for this one.
        fs.io.zero(0, heap_off * SECTOR).map_err(|e| io_err(e, "clearing the partition"))?;
        let tail = (128u64 << 10).min(fs.io.len());
        fs.io.zero(fs.io.len() - tail, tail).map_err(|e| io_err(e, "clearing the partition"))?;

        // System clusters.
        for c in bitmap_first..(root + 1) {
            fs.set_bit(c, true);
        }
        // FAT: media descriptor, reserved, then chains for bitmap, upcase and root.
        fs.fat_set(0, 0xFFFF_FFF8)?;
        fs.fat_set(1, EOC)?;
        for (first, n) in [(bitmap_first, bitmap_clusters), (upcase_first, upcase_clusters), (root, 1)] {
            for i in 0..n {
                fs.fat_set(first + i, if i + 1 == n { EOC } else { first + i + 1 })?;
            }
        }

        // Upcase table.
        let mut up = UPCASE_STD.to_vec();
        up.resize((upcase_clusters as u64 * cluster_bytes) as usize, 0);
        fs.write_cluster_data(upcase_first, &up)?;

        // Root directory: label, bitmap, upcase.
        let mut rootdir = vec![0u8; cluster_bytes as usize];
        if !label.is_empty() {
            rootdir[0] = 0x83;
            rootdir[1] = label.len() as u8;
            for (i, u) in label.iter().enumerate() {
                rootdir[2 + i * 2..4 + i * 2].copy_from_slice(&u.to_le_bytes());
            }
        }
        let e = &mut rootdir[32..64];
        e[0] = 0x81;
        e[20..24].copy_from_slice(&bitmap_first.to_le_bytes());
        e[24..32].copy_from_slice(&bitmap_bytes.to_le_bytes());
        let e = &mut rootdir[64..96];
        e[0] = 0x82;
        e[4..8].copy_from_slice(&checksum32(UPCASE_STD, |_| false).to_le_bytes());
        e[20..24].copy_from_slice(&upcase_first.to_le_bytes());
        e[24..32].copy_from_slice(&(UPCASE_STD.len() as u64).to_le_bytes());
        fs.write_cluster_data(root, &rootdir)?;
        fs.flush_bitmap()?;

        // Boot regions (main + backup).
        let mut region = vec![0u8; 12 * SECTOR as usize];
        {
            let b = &mut region[..512];
            b[0..3].copy_from_slice(&[0xEB, 0x76, 0x90]);
            b[3..11].copy_from_slice(b"EXFAT   ");
            b[64..72].copy_from_slice(&0u64.to_le_bytes()); // PartitionOffset: informational, 0 is accepted
            b[72..80].copy_from_slice(&total.to_le_bytes());
            b[80..84].copy_from_slice(&(fat_off as u32).to_le_bytes());
            b[84..88].copy_from_slice(&(fat_len as u32).to_le_bytes());
            b[88..92].copy_from_slice(&(heap_off as u32).to_le_bytes());
            b[92..96].copy_from_slice(&cluster_count.to_le_bytes());
            b[96..100].copy_from_slice(&root.to_le_bytes());
            b[100..104].copy_from_slice(&p.serial.to_le_bytes());
            b[104..106].copy_from_slice(&0x0100u16.to_le_bytes());
            b[108] = 9;
            b[109] = spc.trailing_zeros() as u8;
            b[110] = 1;
            b[111] = 0x80;
            b[112] = 0xFF;
            b[510] = 0x55;
            b[511] = 0xAA;
        }
        for i in 1..9 {
            let off = i * 512;
            region[off + 508..off + 512].copy_from_slice(&[0, 0, 0x55, 0xAA]);
        }
        let sum = checksum32(&region[..11 * 512], |i| i == 106 || i == 107 || i == 112);
        for i in 0..(512 / 4) {
            region[11 * 512 + i * 4..11 * 512 + i * 4 + 4].copy_from_slice(&sum.to_le_bytes());
        }
        fs.io.write_at(&region, 0).map_err(|e| io_err(e, "writing the filesystem header"))?;
        fs.io.write_at(&region, 12 * SECTOR).map_err(|e| io_err(e, "writing the filesystem header"))?;
        fs.io.sync().map_err(|e| io_err(e, "flushing"))?;
        Ok(fs)
    }

    // -------------------------------------------------------------- open

    pub fn open(io: I) -> Result<Self> {
        let mut b = [0u8; 512];
        io.read_at(&mut b, 0).map_err(|e| io_err(e, "reading the filesystem header"))?;
        if &b[3..11] != b"EXFAT   " || b[510] != 0x55 || b[511] != 0xAA {
            return Err(NyError::new(ErrorKind::InvalidInput, "This is not an exFAT filesystem."));
        }
        if b[108] != 9 {
            return Err(NyError::new(ErrorKind::ImageUnsupported, "Only 512-byte sectors are supported."));
        }
        let u32at = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        let spc = 1u64 << b[109];
        let mut fs = ExFat {
            cluster_bytes: spc * SECTOR,
            fat_off: u32at(80) as u64 * SECTOR,
            heap_off: u32at(88) as u64 * SECTOR,
            cluster_count: u32at(92),
            root: u32at(96),
            bitmap: vec![],
            bitmap_first: 0,
            bitmap_dirty: false,
            upcase: vec![],
            alloc_hint: 2,
            io,
        };
        // Find the bitmap and upcase entries in the root directory.
        let root = fs.root_ref();
        let data = fs.read_dir_bytes(&root)?;
        let mut bm = None;
        let mut up = None;
        for e in data.chunks_exact(32) {
            match e[0] {
                0x00 => break,
                0x81 => bm = Some((u32::from_le_bytes(e[20..24].try_into().unwrap()), u64::from_le_bytes(e[24..32].try_into().unwrap()))),
                0x82 => up = Some((u32::from_le_bytes(e[20..24].try_into().unwrap()), u64::from_le_bytes(e[24..32].try_into().unwrap()))),
                _ => {}
            }
        }
        let (bf, bl) = bm.ok_or_else(|| corrupt("no allocation bitmap"))?;
        let (uf, ul) = up.ok_or_else(|| corrupt("no upcase table"))?;
        fs.bitmap_first = bf;
        fs.bitmap = fs.read_contiguous(bf, bl)?;
        let table = fs.read_contiguous(uf, ul)?;
        fs.upcase = decode_upcase(&table);
        Ok(fs)
    }

    pub fn into_io(self) -> I {
        self.io
    }

    // ----------------------------------------------------------- geometry

    pub fn cluster_bytes(&self) -> u64 {
        self.cluster_bytes
    }

    pub fn cluster_count(&self) -> u32 {
        self.cluster_count
    }

    pub fn free_bytes(&self) -> u64 {
        let used = (0..self.cluster_count).filter(|&i| self.bitmap[i as usize / 8] >> (i % 8) & 1 == 1).count() as u64;
        (self.cluster_count as u64 - used) * self.cluster_bytes
    }

    fn cluster_off(&self, c: u32) -> u64 {
        self.heap_off + (c as u64 - 2) * self.cluster_bytes
    }

    fn root_ref(&self) -> DirRef {
        DirRef { first: self.root, len: 0, no_fat_chain: false, is_root: true }
    }

    fn valid_cluster(&self, c: u32) -> bool {
        c >= 2 && c < self.cluster_count + 2
    }

    // ---------------------------------------------------------------- FAT

    fn fat_get(&self, c: u32) -> Result<u32> {
        let mut b = [0u8; 4];
        self.io.read_at(&mut b, self.fat_off + c as u64 * 4).map_err(|e| io_err(e, "reading the FAT"))?;
        Ok(u32::from_le_bytes(b))
    }

    fn fat_set(&self, c: u32, v: u32) -> Result<()> {
        self.io.write_at(&v.to_le_bytes(), self.fat_off + c as u64 * 4).map_err(|e| io_err(e, "writing the FAT"))
    }

    fn chain(&self, first: u32) -> Result<Vec<u32>> {
        let mut out = vec![];
        let mut c = first;
        while c != EOC && c != 0 {
            if !self.valid_cluster(c) || out.len() > self.cluster_count as usize {
                return Err(corrupt("a cluster chain leaves the volume or loops"));
            }
            out.push(c);
            c = self.fat_get(c)?;
        }
        Ok(out)
    }

    // ------------------------------------------------------------- bitmap

    fn bit(&self, c: u32) -> bool {
        let i = (c - 2) as usize;
        self.bitmap[i / 8] >> (i % 8) & 1 == 1
    }

    fn set_bit(&mut self, c: u32, on: bool) {
        let i = (c - 2) as usize;
        if on {
            self.bitmap[i / 8] |= 1 << (i % 8);
        } else {
            self.bitmap[i / 8] &= !(1 << (i % 8));
        }
        self.bitmap_dirty = true;
    }

    fn flush_bitmap(&mut self) -> Result<()> {
        if !self.bitmap_dirty {
            return Ok(());
        }
        let off = self.cluster_off(self.bitmap_first);
        // Whole sectors only.
        let mut buf = self.bitmap.clone();
        buf.resize(buf.len().next_multiple_of(SECTOR as usize), 0);
        self.io.write_at(&buf, off).map_err(|e| io_err(e, "writing the free-space map"))?;
        self.bitmap_dirty = false;
        Ok(())
    }

    /// First run of `n` free clusters, or None.
    fn find_free_run(&self, n: u32) -> Option<u32> {
        if n == 0 {
            return Some(2);
        }
        let total = self.cluster_count;
        for start_hint in [self.alloc_hint.max(2), 2] {
            let mut run = 0u32;
            let mut c = start_hint;
            let limit = if start_hint == 2 { total + 2 } else { total + 2 };
            while c < limit {
                if self.bit(c) {
                    run = 0;
                } else {
                    run += 1;
                    if run == n {
                        return Some(c + 1 - n);
                    }
                }
                c += 1;
            }
        }
        None
    }

    fn alloc_run(&mut self, n: u32) -> Result<u32> {
        let first = self.find_free_run(n).ok_or_else(|| {
            NyError::new(ErrorKind::NoSpace, "There is no room left on the NyBoot data partition for that.")
                .with_hint("Free some space, or use a bigger drive. (Files need one unbroken stretch of free space.)")
        })?;
        for c in first..first + n {
            self.set_bit(c, true);
        }
        self.alloc_hint = first + n;
        Ok(first)
    }

    fn free_run(&mut self, first: u32, n: u32) {
        for c in first..first + n {
            if self.valid_cluster(c) {
                self.set_bit(c, false);
            }
        }
        self.alloc_hint = self.alloc_hint.min(first).max(2);
    }

    // ---------------------------------------------------------- raw data

    fn write_cluster_data(&self, first: u32, data: &[u8]) -> Result<()> {
        self.io.write_at(data, self.cluster_off(first)).map_err(|e| io_err(e, "writing to the drive"))
    }

    fn read_contiguous(&self, first: u32, len: u64) -> Result<Vec<u8>> {
        if !self.valid_cluster(first) {
            return Err(corrupt("a system file points outside the volume"));
        }
        let mut v = vec![0u8; len.next_multiple_of(SECTOR) as usize];
        self.io.read_at(&mut v, self.cluster_off(first)).map_err(|e| io_err(e, "reading the drive"))?;
        v.truncate(len as usize);
        Ok(v)
    }

    // -------------------------------------------------------- directories

    fn dir_clusters(&self, d: &DirRef) -> Result<Vec<u32>> {
        if d.no_fat_chain {
            let n = d.len.div_ceil(self.cluster_bytes) as u32;
            Ok((d.first..d.first + n).collect())
        } else {
            self.chain(d.first)
        }
    }

    fn read_dir_bytes(&self, d: &DirRef) -> Result<Vec<u8>> {
        let cl = self.dir_clusters(d)?;
        let mut v = vec![0u8; cl.len() * self.cluster_bytes as usize];
        for (i, c) in cl.iter().enumerate() {
            let o = i * self.cluster_bytes as usize;
            self.io
                .read_at(&mut v[o..o + self.cluster_bytes as usize], self.cluster_off(*c))
                .map_err(|e| io_err(e, "reading a folder"))?;
        }
        Ok(v)
    }

    fn write_dir_range(&self, d: &DirRef, bytes: &[u8], start: usize) -> Result<()> {
        // Write `bytes` at byte offset `start` of the directory, cluster by cluster.
        let cl = self.dir_clusters(d)?;
        let cb = self.cluster_bytes as usize;
        let mut pos = start;
        let mut rest = bytes;
        while !rest.is_empty() {
            let ci = pos / cb;
            let within = pos % cb;
            let n = rest.len().min(cb - within);
            let c = *cl.get(ci).ok_or_else(|| corrupt("directory write past its end"))?;
            self.io.write_at(&rest[..n], self.cluster_off(c) + within as u64).map_err(|e| io_err(e, "writing a folder"))?;
            pos += n;
            rest = &rest[n..];
        }
        Ok(())
    }

    fn parse_dir(&self, raw: &[u8]) -> Vec<Entry> {
        let mut out = vec![];
        let mut i = 0;
        while i + 32 <= raw.len() {
            let t = raw[i];
            if t == 0x00 {
                break;
            }
            if t == 0x85 {
                let sec = raw[i + 1] as usize;
                if sec >= 2 && i + 32 * (sec + 1) <= raw.len() && raw[i + 32] == 0xC0 {
                    let attrs = u16::from_le_bytes([raw[i + 4], raw[i + 5]]);
                    let s = &raw[i + 32..i + 64];
                    let name_len = s[3] as usize;
                    let mut units: Vec<u16> = vec![];
                    for k in 2..=sec {
                        let n = &raw[i + 32 * k..i + 32 * k + 32];
                        if n[0] != 0xC1 {
                            break;
                        }
                        for u in n[2..32].chunks_exact(2) {
                            units.push(u16::from_le_bytes([u[0], u[1]]));
                        }
                    }
                    units.truncate(name_len);
                    out.push(Entry {
                        name: String::from_utf16_lossy(&units),
                        is_dir: attrs & ATTR_DIR != 0,
                        first_cluster: u32::from_le_bytes(s[20..24].try_into().unwrap()),
                        size: u64::from_le_bytes(s[24..32].try_into().unwrap()),
                        valid_size: u64::from_le_bytes(s[8..16].try_into().unwrap()),
                        no_fat_chain: s[1] & 0x02 != 0,
                        loc: (i, sec + 1),
                    });
                }
                i += 32 * (sec + 1);
            } else {
                i += 32;
            }
        }
        out
    }

    fn upcase_str(&self, s: &str) -> Vec<u16> {
        s.encode_utf16().map(|c| self.upcase.get(c as usize).copied().unwrap_or(c)).collect()
    }

    fn name_hash(&self, name: &str) -> u16 {
        let mut h: u16 = 0;
        for u in self.upcase_str(name) {
            for b in u.to_le_bytes() {
                h = (if h & 1 != 0 { 0x8000u16 } else { 0u16 }).wrapping_add(h >> 1).wrapping_add(b as u16);
            }
        }
        h
    }

    fn same_name(&self, a: &str, b: &str) -> bool {
        self.upcase_str(a) == self.upcase_str(b)
    }

    fn dir_of(&self, e: &Entry) -> DirRef {
        DirRef { first: e.first_cluster, len: e.size, no_fat_chain: e.no_fat_chain, is_root: false }
    }

    /// Walk a `/`-separated path to its directory (all components must be folders).
    fn walk_dir(&self, path: &[&str]) -> Result<DirRef> {
        let mut cur = self.root_ref();
        for part in path {
            let entries = self.parse_dir(&self.read_dir_bytes(&cur)?);
            let e = entries
                .iter()
                .find(|e| self.same_name(&e.name, part))
                .ok_or_else(|| NyError::new(ErrorKind::InvalidInput, format!("The folder \"{part}\" does not exist on the drive.")))?;
            if !e.is_dir {
                return Err(NyError::new(ErrorKind::InvalidInput, format!("\"{part}\" is a file, not a folder.")));
            }
            cur = self.dir_of(e);
        }
        Ok(cur)
    }

    fn split(path: &str) -> Vec<&str> {
        path.split('/').filter(|s| !s.is_empty()).collect()
    }

    // ---------------------------------------------------------- public API

    pub fn list(&self, path: &str) -> Result<Vec<Entry>> {
        let d = self.walk_dir(&Self::split(path))?;
        Ok(self.parse_dir(&self.read_dir_bytes(&d)?))
    }

    pub fn stat(&self, path: &str) -> Result<Option<Entry>> {
        let parts = Self::split(path);
        let Some((name, dirs)) = parts.split_last() else { return Ok(None) };
        let d = self.walk_dir(dirs)?;
        Ok(self.parse_dir(&self.read_dir_bytes(&d)?).into_iter().find(|e| self.same_name(&e.name, name)))
    }

    pub fn label(&self) -> Result<String> {
        let data = self.read_dir_bytes(&self.root_ref())?;
        for e in data.chunks_exact(32) {
            if e[0] == 0x00 {
                break;
            }
            if e[0] == 0x83 {
                let n = (e[1] as usize).min(11);
                let units: Vec<u16> = e[2..2 + n * 2].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                return Ok(String::from_utf16_lossy(&units));
            }
        }
        Ok(String::new())
    }

    /// Physical extents of a file: `(partition_byte_offset, bytes)` pairs.
    pub fn extents(&self, e: &Entry) -> Result<Vec<(u64, u64)>> {
        if e.size == 0 {
            return Ok(vec![]);
        }
        if e.no_fat_chain {
            return Ok(vec![(self.cluster_off(e.first_cluster), e.size)]);
        }
        let mut out: Vec<(u64, u64)> = vec![];
        let mut left = e.size;
        for c in self.chain(e.first_cluster)? {
            let n = left.min(self.cluster_bytes);
            let off = self.cluster_off(c);
            match out.last_mut() {
                Some((o, l)) if *o + *l == off => *l += n,
                _ => out.push((off, n)),
            }
            left -= n;
        }
        Ok(out)
    }

    pub fn read_file(&self, e: &Entry, offset: u64, buf: &mut [u8]) -> Result<usize> {
        if e.is_dir {
            return Err(NyError::invalid("That is a folder."));
        }
        if offset >= e.size {
            return Ok(0);
        }
        let want = (buf.len() as u64).min(e.size - offset) as usize;
        let mut done = 0usize;
        let mut pos = 0u64;
        for (off, len) in self.extents(e)? {
            if offset + (done as u64) >= pos + len {
                pos += len;
                continue;
            }
            let skip = offset + done as u64 - pos;
            let n = ((len - skip) as usize).min(want - done);
            self.io.read_at(&mut buf[done..done + n], off + skip).map_err(|e| io_err(e, "reading the drive"))?;
            done += n;
            pos += len;
            if done == want {
                break;
            }
        }
        Ok(done)
    }

    pub fn mkdir_p(&mut self, path: &str) -> Result<()> {
        let parts = Self::split(path);
        for i in 0..parts.len() {
            let parent = self.walk_dir(&parts[..i])?;
            let existing = self.parse_dir(&self.read_dir_bytes(&parent)?);
            match existing.iter().find(|e| self.same_name(&e.name, parts[i])) {
                Some(e) if e.is_dir => continue,
                Some(_) => {
                    return Err(NyError::new(ErrorKind::InvalidInput, format!("\"{}\" already exists as a file.", parts[i])));
                }
                None => {
                    self.create_dir(&parent, parts[i])?;
                }
            }
        }
        Ok(())
    }

    fn build_entry_set(&self, name: &str, attrs: u16, first: u32, size: u64, valid: u64, no_fat_chain: bool) -> Vec<u8> {
        let units: Vec<u16> = name.encode_utf16().collect();
        let name_entries = units.len().div_ceil(15);
        let mut set = vec![0u8; 32 * (2 + name_entries)];
        let ts = dos_timestamp();
        {
            let p = &mut set[0..32];
            p[0] = 0x85;
            p[1] = (1 + name_entries) as u8;
            p[4..6].copy_from_slice(&attrs.to_le_bytes());
            p[8..12].copy_from_slice(&ts.to_le_bytes());
            p[12..16].copy_from_slice(&ts.to_le_bytes());
            p[16..20].copy_from_slice(&ts.to_le_bytes());
            p[22] = 0x80;
            p[23] = 0x80;
            p[24] = 0x80;
        }
        {
            let s = &mut set[32..64];
            s[0] = 0xC0;
            s[1] = 0x01 | if no_fat_chain { 0x02 } else { 0 };
            s[3] = units.len() as u8;
            s[4..6].copy_from_slice(&self.name_hash(name).to_le_bytes());
            s[8..16].copy_from_slice(&valid.to_le_bytes());
            s[20..24].copy_from_slice(&first.to_le_bytes());
            s[24..32].copy_from_slice(&size.to_le_bytes());
        }
        for (k, chunk) in units.chunks(15).enumerate() {
            let n = &mut set[64 + 32 * k..96 + 32 * k];
            n[0] = 0xC1;
            for (i, u) in chunk.iter().enumerate() {
                n[2 + i * 2..4 + i * 2].copy_from_slice(&u.to_le_bytes());
            }
        }
        let sum = set_checksum(&set);
        set[2..4].copy_from_slice(&sum.to_le_bytes());
        set
    }

    /// Put an entry set into a directory (extending the root if it is full).
    fn insert_entry_set(&mut self, dir: &DirRef, set: &[u8]) -> Result<()> {
        let raw = self.read_dir_bytes(dir)?;
        let need = set.len() / 32;
        // First place with `need` consecutive free (0x00 or deleted) entries.
        let mut run = 0usize;
        let mut at = None;
        let n_entries = raw.len() / 32;
        let mut seen_end = false;
        for i in 0..n_entries {
            let t = raw[i * 32];
            let free = t == 0x00 || t & 0x80 == 0;
            if t == 0x00 {
                seen_end = true;
            }
            if free || seen_end {
                run += 1;
                if run == need {
                    at = Some((i + 1 - need) * 32);
                    break;
                }
            } else {
                run = 0;
            }
        }
        let at = match at {
            Some(a) => a,
            None => {
                // Directory full.
                if !dir.is_root {
                    return Err(NyError::new(ErrorKind::NoSpace, "That folder on the drive is full.")
                        .with_hint("Put the file in the top level or another folder."));
                }
                let new = self.alloc_run(1)?;
                self.write_cluster_data(new, &vec![0u8; self.cluster_bytes as usize])?;
                let chain = self.chain(dir.first)?;
                self.fat_set(*chain.last().unwrap(), new)?;
                self.fat_set(new, EOC)?;
                self.flush_bitmap()?;
                // The free run at the end of the old directory continues into the new cluster.
                if run * 32 + self.cluster_bytes as usize <= (need - 1) * 32 {
                    return Err(NyError::other("internal error: directory extension too small"));
                }
                (n_entries - run) * 32
            }
        };
        self.write_dir_range(dir, set, at)
    }

    fn create_dir(&mut self, parent: &DirRef, name: &str) -> Result<()> {
        if let Some(msg) = invalid_name(name) {
            return Err(NyError::invalid(msg));
        }
        let c = self.alloc_run(1)?;
        self.write_cluster_data(c, &vec![0u8; self.cluster_bytes as usize])?;
        self.fat_set(c, EOC)?;
        self.flush_bitmap()?;
        let set = self.build_entry_set(name, ATTR_DIR, c, self.cluster_bytes, self.cluster_bytes, false);
        self.insert_entry_set(parent, &set)
    }

    /// Add a file. `progress` receives the bytes written so far and returns false to cancel.
    pub fn add_file(&mut self, path: &str, reader: &mut dyn Read, len: u64, progress: &mut dyn FnMut(u64) -> bool) -> Result<Entry> {
        let parts = Self::split(path);
        let Some((name, dirs)) = parts.split_last() else {
            return Err(NyError::invalid("No file name given."));
        };
        if let Some(msg) = invalid_name(name) {
            return Err(NyError::invalid(msg));
        }
        if !dirs.is_empty() {
            self.mkdir_p(&dirs.join("/"))?;
        }
        let dir = self.walk_dir(dirs)?;
        let existing = self.parse_dir(&self.read_dir_bytes(&dir)?);
        if existing.iter().any(|e| self.same_name(&e.name, name)) {
            return Err(NyError::new(ErrorKind::InvalidInput, format!("\"{name}\" is already on the drive.")).with_hint("Remove it first, or rename the file."));
        }

        let clusters = len.div_ceil(self.cluster_bytes) as u32;
        let first = if clusters == 0 { 0 } else { self.alloc_run(clusters)? };
        self.flush_bitmap()?;

        // Stream the data in big sector-aligned pieces.
        let chunk = 4usize << 20;
        let mut buf = vec![0u8; chunk];
        let mut written = 0u64;
        let base = if clusters == 0 { 0 } else { self.cluster_off(first) };
        while written < len {
            let want = ((len - written) as usize).min(chunk);
            let mut got = 0;
            while got < want {
                match reader.read(&mut buf[got..want]) {
                    Ok(0) => break,
                    Ok(n) => got += n,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) => {
                        self.free_run(first, clusters);
                        let _ = self.flush_bitmap();
                        return Err(NyError::from_io(&e, "reading the file to add"));
                    }
                }
            }
            if got < want {
                self.free_run(first, clusters);
                let _ = self.flush_bitmap();
                return Err(NyError::new(ErrorKind::ImageCorrupt, "The file ended before its expected size."));
            }
            let padded = want.next_multiple_of(SECTOR as usize);
            buf[want..padded].fill(0);
            if let Err(e) = self.io.write_at(&buf[..padded], base + written) {
                self.free_run(first, clusters);
                let _ = self.flush_bitmap();
                return Err(io_err(e, "writing to the drive"));
            }
            written += want as u64;
            if !progress(written) {
                self.free_run(first, clusters);
                let _ = self.flush_bitmap();
                return Err(NyError::cancelled());
            }
        }
        self.io.sync().map_err(|e| io_err(e, "flushing"))?;

        // An empty file owns no clusters, so it has no contiguous run to flag.
        let set = self.build_entry_set(name, ATTR_ARCHIVE, first, len, len, clusters > 0);
        if let Err(e) = self.insert_entry_set(&dir, &set) {
            self.free_run(first, clusters);
            let _ = self.flush_bitmap();
            return Err(e);
        }
        self.io.sync().map_err(|e| io_err(e, "flushing"))?;
        self.stat(path)?.ok_or_else(|| NyError::other("file vanished after writing"))
    }

    pub fn remove(&mut self, path: &str) -> Result<()> {
        let parts = Self::split(path);
        let Some((name, dirs)) = parts.split_last() else {
            return Err(NyError::invalid("No file name given."));
        };
        let dir = self.walk_dir(dirs)?;
        let entries = self.parse_dir(&self.read_dir_bytes(&dir)?);
        let e = entries
            .iter()
            .find(|e| self.same_name(&e.name, name))
            .ok_or_else(|| NyError::new(ErrorKind::InvalidInput, format!("\"{name}\" is not on the drive.")))?
            .clone();
        if e.is_dir && !self.list(&parts.join("/"))?.is_empty() {
            return Err(NyError::new(ErrorKind::InvalidInput, format!("The folder \"{name}\" is not empty.")));
        }
        // Mark the entry set as unused first, then give the space back.
        let mut raw = self.read_dir_bytes(&dir)?;
        for k in 0..e.loc.1 {
            raw[e.loc.0 + k * 32] &= 0x7F;
        }
        let range = &raw[e.loc.0..e.loc.0 + e.loc.1 * 32];
        self.write_dir_range(&dir, range, e.loc.0)?;
        if e.first_cluster != 0 {
            if e.no_fat_chain {
                self.free_run(e.first_cluster, e.size.div_ceil(self.cluster_bytes) as u32);
            } else {
                for c in self.chain(e.first_cluster)? {
                    self.fat_set(c, 0)?;
                    self.free_run(c, 1);
                }
            }
        }
        self.flush_bitmap()?;
        self.io.sync().map_err(|e| io_err(e, "flushing"))
    }

    pub fn sync(&mut self) -> Result<()> {
        self.flush_bitmap()?;
        self.io.sync().map_err(|e| io_err(e, "flushing"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nyboot::io::FileIo;
    use std::process::Command;

    fn tdir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("nyforge-exfat-{}-{}", std::process::id(), name));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn make_image(path: &std::path::Path, mib: u64) -> FileIo {
        let f = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(path).unwrap();
        f.set_len(mib << 20).unwrap();
        FileIo::new(f, 0, mib << 20, 1)
    }

    /// Run the reference checker if it is installed. Returns false when it isn't.
    fn fsck(path: &std::path::Path) -> bool {
        match Command::new("fsck.exfat").arg(path).output() {
            Ok(o) => {
                assert!(o.status.success(), "fsck.exfat failed:\n{}\n{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
                true
            }
            Err(_) => false,
        }
    }

    fn pattern(n: usize, seed: usize) -> Vec<u8> {
        (0..n).map(|i| ((i.wrapping_mul(2654435761) >> 5) ^ seed) as u8).collect()
    }

    fn add(fs: &mut ExFat<FileIo>, path: &str, data: &[u8]) -> Entry {
        fs.add_file(path, &mut &data[..], data.len() as u64, &mut |_| true).unwrap()
    }

    fn read_back(fs: &ExFat<FileIo>, path: &str) -> Vec<u8> {
        let e = fs.stat(path).unwrap().unwrap();
        let mut v = vec![0u8; e.size as usize];
        assert_eq!(fs.read_file(&e, 0, &mut v).unwrap(), v.len());
        v
    }

    #[test]
    fn fresh_format_passes_fsck() {
        let d = tdir("fresh");
        let img = d.join("a.img");
        let fs = ExFat::format(make_image(&img, 64), &FormatParams { label: "NYBOOT".into(), serial: 0x1234abcd, cluster_bytes: None }).unwrap();
        assert_eq!(fs.label().unwrap(), "NYBOOT");
        assert!(fs.list("/").unwrap().is_empty());
        drop(fs);
        if !fsck(&img) {
            eprintln!("(fsck.exfat not installed: skipped)");
        }
    }

    #[test]
    fn files_of_awkward_sizes_roundtrip_and_pass_fsck() {
        let d = tdir("sizes");
        let img = d.join("a.img");
        let mut fs = ExFat::format(make_image(&img, 96), &FormatParams { label: "T".into(), serial: 1, cluster_bytes: Some(4096) }).unwrap();
        let sizes = [0usize, 1, 511, 512, 513, 4095, 4096, 4097, 100_000, 1 << 20, (3 << 20) + 17];
        for (i, &n) in sizes.iter().enumerate() {
            add(&mut fs, &format!("f{i}.bin"), &pattern(n, i));
        }
        add(&mut fs, "iso/linux/deep file ünï.iso", &pattern(70_000, 99));
        for (i, &n) in sizes.iter().enumerate() {
            assert_eq!(read_back(&fs, &format!("F{i}.BIN")), pattern(n, i), "size {n} (also checks case-insensitive lookup)");
        }
        assert_eq!(read_back(&fs, "/iso/linux/deep file ünï.iso"), pattern(70_000, 99));
        assert_eq!(fs.list("/").unwrap().len(), sizes.len() + 1);
        let e = fs.stat("f9.bin").unwrap().unwrap();
        let ex = fs.extents(&e).unwrap();
        assert_eq!(ex.len(), 1);
        assert_eq!(ex[0].1, 1 << 20);
        fs.sync().unwrap();
        drop(fs);
        fsck(&img);
    }

    #[test]
    fn remove_frees_space_and_stays_consistent() {
        let d = tdir("remove");
        let img = d.join("a.img");
        let mut fs = ExFat::format(make_image(&img, 64), &FormatParams { label: "T".into(), serial: 2, cluster_bytes: Some(4096) }).unwrap();
        let free0 = fs.free_bytes();
        add(&mut fs, "big.bin", &pattern(20 << 20, 1));
        add(&mut fs, "dir/small.bin", &pattern(5000, 2));
        assert!(fs.free_bytes() < free0 - (20 << 20));
        assert!(fs.remove("dir").is_err(), "non-empty folder must not be removed");
        fs.remove("dir/small.bin").unwrap();
        fs.remove("dir").unwrap();
        fs.remove("BIG.bin").unwrap();
        assert_eq!(fs.free_bytes(), free0);
        assert!(fs.list("/").unwrap().is_empty());
        // The freed space is reusable, and a name can come back.
        add(&mut fs, "big.bin", &pattern(30 << 20, 3));
        assert_eq!(read_back(&fs, "big.bin"), pattern(30 << 20, 3));
        drop(fs);
        fsck(&img);
    }

    #[test]
    fn full_drive_and_bad_names_are_refused_cleanly() {
        let d = tdir("full");
        let img = d.join("a.img");
        let mut fs = ExFat::format(make_image(&img, 16), &FormatParams { label: "T".into(), serial: 3, cluster_bytes: Some(4096) }).unwrap();
        let big = pattern(20 << 20, 0);
        let e = fs.add_file("big.bin", &mut &big[..], big.len() as u64, &mut |_| true).unwrap_err();
        assert_eq!(e.kind, ErrorKind::NoSpace);
        for bad in ["a:b", "a/..", "x?y", "end.", "tab\there"] {
            let r = fs.add_file(bad, &mut &b"x"[..], 1, &mut |_| true);
            assert!(r.is_err(), "{bad:?} should be rejected");
        }
        add(&mut fs, "ok.txt", b"hi");
        let e = fs.add_file("OK.TXT", &mut &b"x"[..], 1, &mut |_| true).unwrap_err();
        assert!(e.message.contains("already"), "{}", e.message);
        drop(fs);
        fsck(&img);
    }

    #[test]
    fn root_directory_grows_past_one_cluster() {
        let d = tdir("grow");
        let img = d.join("a.img");
        let mut fs = ExFat::format(make_image(&img, 64), &FormatParams { label: "T".into(), serial: 4, cluster_bytes: Some(4096) }).unwrap();
        // 4 KiB cluster = 128 entries; each short-name file uses 3.
        for i in 0..90 {
            add(&mut fs, &format!("file-{i:03}.txt"), format!("content {i}").as_bytes());
        }
        assert_eq!(fs.list("/").unwrap().len(), 90);
        for i in [0, 41, 42, 89] {
            assert_eq!(read_back(&fs, &format!("file-{i:03}.txt")), format!("content {i}").into_bytes());
        }
        drop(fs);
        fsck(&img);
    }

    #[test]
    fn reopen_keeps_everything() {
        let d = tdir("reopen");
        let img = d.join("a.img");
        {
            let mut fs = ExFat::format(make_image(&img, 64), &FormatParams { label: "KEEP".into(), serial: 5, cluster_bytes: None }).unwrap();
            add(&mut fs, "a/b/c.bin", &pattern(123_456, 7));
            fs.sync().unwrap();
        }
        let f = std::fs::OpenOptions::new().read(true).write(true).open(&img).unwrap();
        let fs = ExFat::open(FileIo::new(f, 0, 64 << 20, 1)).unwrap();
        assert_eq!(fs.label().unwrap(), "KEEP");
        assert_eq!(read_back(&fs, "A/B/C.BIN"), pattern(123_456, 7));
    }

    #[test]
    fn can_work_inside_a_filesystem_made_by_mkfs_exfat() {
        let d = tdir("foreign");
        let img = d.join("ref.img");
        std::fs::File::create(&img).unwrap().set_len(64 << 20).unwrap();
        match Command::new("mkfs.exfat").args(["-L", "REF"]).arg(&img).output() {
            Ok(o) if o.status.success() => {}
            _ => {
                eprintln!("(mkfs.exfat not installed: skipped)");
                return;
            }
        }
        let f = std::fs::OpenOptions::new().read(true).write(true).open(&img).unwrap();
        let mut fs = ExFat::open(FileIo::new(f, 0, 64 << 20, 1)).unwrap();
        assert_eq!(fs.label().unwrap(), "REF");
        add(&mut fs, "x/y.bin", &pattern(1_000_000, 11));
        add(&mut fs, "z.bin", &pattern(77, 12));
        fs.remove("z.bin").unwrap();
        assert_eq!(read_back(&fs, "x/y.bin"), pattern(1_000_000, 11));
        drop(fs);
        fsck(&img);
    }

    #[test]
    fn upcase_table_decodes_to_unicode_case_rules() {
        let t = decode_upcase(UPCASE_STD);
        assert_eq!(t.len(), 65536);
        assert_eq!(t['a' as usize], 'A' as u16);
        assert_eq!(t['ä' as usize], 'Ä' as u16);
        assert_eq!(t['0' as usize], '0' as u16);
        assert_eq!(t['ж' as usize], 'Ж' as u16);
    }
}
