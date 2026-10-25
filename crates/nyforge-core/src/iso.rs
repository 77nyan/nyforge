//! Read-only access to the file tree inside an ISO: ISO 9660 (with Rock Ridge
//! and Joliet long names) and UDF (what Windows installers use).
//!
//! Used to look inside an image before it is written (is it a Windows
//! installer? does it carry a `loopback.cfg`? which distro family?), and by
//! the Windows install-media mode to copy the files out.
//!
//! Limits, stated honestly: UDF with the "metadata partition" (UDF 2.50, mostly
//! Blu-ray) and "virtual partition" (CD-R packet writing) is not supported;
//! files are read, never modified; no El Torito parsing.

use crate::error::{ErrorKind, NyError, Result};
use crate::sys::read_exact_at;
use std::fs::File;
use std::path::Path;

const SECTOR: u64 = 2048;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Iso9660,
    Udf,
    /// Both (e.g. Windows installers): the UDF tree is the complete one.
    Hybrid,
}

#[derive(Debug, Clone)]
enum Content {
    /// `(absolute byte offset in the image, length)` pieces, in file order.
    Extents(Vec<(u64, u64)>),
    /// Small UDF files/directories stored inside their descriptor.
    Inline(Vec<u8>),
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    content: Content,
}

enum Backend {
    Iso { root: Content, joliet: bool },
    Udf(Udf),
}

pub struct IsoImage {
    file: File,
    backend: Backend,
    pub kind: Kind,
    pub label: String,
    pub total_bytes: u64,
}

fn bad(msg: impl Into<String>) -> NyError {
    NyError::new(ErrorKind::ImageCorrupt, format!("This ISO's file system is damaged: {}", msg.into()))
}

fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn u64le(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

fn ucs2be(b: &[u8]) -> String {
    let u: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
    String::from_utf16_lossy(&u)
}

impl IsoImage {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).map_err(|e| NyError::from_io(&e, &format!("opening {}", path.display())))?;
        let total_bytes = file.metadata().map_err(|e| NyError::from_io(&e, "reading the file size"))?.len();
        let mut me = IsoImage {
            file,
            backend: Backend::Iso { root: Content::Inline(vec![]), joliet: false },
            kind: Kind::Iso9660,
            label: String::new(),
            total_bytes,
        };
        let iso = me.read_iso9660();
        let udf = me.read_udf();
        match (iso, udf) {
            (Ok((root, joliet, label)), Ok(u)) => {
                me.label = if u.label.is_empty() { label } else { u.label.clone() };
                me.backend = Backend::Udf(u);
                me.kind = Kind::Hybrid;
                let _ = (root, joliet);
            }
            (Err(_), Ok(u)) => {
                me.label = u.label.clone();
                me.backend = Backend::Udf(u);
                me.kind = Kind::Udf;
            }
            (Ok((root, joliet, label)), Err(_)) => {
                me.label = label;
                me.backend = Backend::Iso { root, joliet };
                me.kind = Kind::Iso9660;
            }
            (Err(e), Err(_)) => return Err(e),
        }
        Ok(me)
    }

    fn read_at(&self, off: u64, len: usize) -> Result<Vec<u8>> {
        let mut v = vec![0u8; len];
        read_exact_at(&self.file, &mut v, off).map_err(|e| {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                bad("it ends in the middle of a structure")
            } else {
                NyError::from_io(&e, "reading the ISO")
            }
        })?;
        Ok(v)
    }

    // ------------------------------------------------------------ ISO 9660

    fn read_iso9660(&self) -> Result<(Content, bool, String)> {
        let mut primary: Option<Vec<u8>> = None;
        let mut joliet: Option<Vec<u8>> = None;
        for i in 16..64u64 {
            if (i + 1) * SECTOR > self.total_bytes {
                break;
            }
            let d = self.read_at(i * SECTOR, 2048)?;
            if &d[1..6] != b"CD001" {
                break;
            }
            match d[0] {
                1 if primary.is_none() => primary = Some(d),
                2 => {
                    let esc = &d[88..91];
                    if esc == b"%/@" || esc == b"%/C" || esc == b"%/E" {
                        joliet = Some(d);
                    }
                }
                255 => break,
                _ => {}
            }
        }
        let p = primary.ok_or_else(|| bad("no ISO 9660 volume descriptor"))?;
        let label = String::from_utf8_lossy(&p[40..72]).trim().to_string();
        let (d, is_j) = match &joliet {
            Some(j) => (j, true),
            None => (&p, false),
        };
        let rec = &d[156..190];
        let root = self.iso_content(rec, SECTOR)?;
        Ok((root, is_j, label))
    }

    fn iso_content(&self, rec: &[u8], bs: u64) -> Result<Content> {
        let lba = u32le(rec, 2) as u64;
        let len = u32le(rec, 10) as u64;
        Ok(Content::Extents(vec![(lba * bs, len)]))
    }

    fn iso_list(&self, dir: &Content, joliet: bool) -> Result<Vec<Entry>> {
        let raw = self.content_bytes(dir, u64::MAX)?;
        let mut out: Vec<Entry> = vec![];
        let mut pos = 0usize;
        let mut pending: Option<(String, bool, Vec<(u64, u64)>)> = None; // multi-extent file
        while pos < raw.len() {
            let len = raw[pos] as usize;
            if len == 0 {
                // Records never straddle sectors: skip padding to the next one.
                pos = (pos / SECTOR as usize + 1) * SECTOR as usize;
                continue;
            }
            if pos + len > raw.len() || len < 34 {
                break;
            }
            let r = &raw[pos..pos + len];
            pos += len;
            let flags = r[25];
            let nlen = r[32] as usize;
            if 33 + nlen > len {
                break;
            }
            let idb = &r[33..33 + nlen];
            if idb == [0] || idb == [1] {
                continue; // "." and ".."
            }
            let mut name = if joliet { ucs2be(idb) } else { String::from_utf8_lossy(idb).into_owned() };
            if !joliet {
                // Rock Ridge alternate name, when present.
                let su_start = 33 + nlen + if nlen % 2 == 0 { 1 } else { 0 };
                if let Some(nm) = rock_ridge_name(r.get(su_start..).unwrap_or(&[])) {
                    name = nm;
                } else if let Some(i) = name.rfind(';') {
                    name.truncate(i);
                    if name.ends_with('.') {
                        name.pop();
                    }
                }
            } else if let Some(i) = name.rfind(';') {
                name.truncate(i);
            }
            let is_dir = flags & 2 != 0;
            let ext = (u32le(r, 2) as u64 * SECTOR, u32le(r, 10) as u64);
            if flags & 0x80 != 0 {
                match &mut pending {
                    Some((_, _, v)) => v.push(ext),
                    None => pending = Some((name, is_dir, vec![ext])),
                }
                continue;
            }
            let (name, exts) = match pending.take() {
                Some((n, _, mut v)) => {
                    v.push(ext);
                    (n, v)
                }
                None => (name, vec![ext]),
            };
            let size = exts.iter().map(|e| e.1).sum();
            out.push(Entry { name, is_dir, size, content: Content::Extents(exts) });
        }
        Ok(out)
    }

    // --------------------------------------------------------------- UDF

    fn read_udf(&self) -> Result<Udf> {
        // The anchor lives at sector 256 (and near the end).
        let mut anchor = None;
        let last = (self.total_bytes / SECTOR).saturating_sub(1);
        for s in [256u64, last.saturating_sub(256), last] {
            if s * SECTOR + 512 > self.total_bytes {
                continue;
            }
            let d = self.read_at(s * SECTOR, 512)?;
            if u16le(&d, 0) == 2 {
                anchor = Some(d);
                break;
            }
        }
        let a = anchor.ok_or_else(|| bad("no UDF anchor"))?;
        let vds_len = u32le(&a, 16) as u64;
        let vds_loc = u32le(&a, 20) as u64;
        let mut parts: Vec<(u16, u64)> = vec![]; // (partition number, start sector)
        let mut lvd: Option<Vec<u8>> = None;
        let mut label = String::new();
        let nsec = (vds_len / SECTOR).clamp(1, 64);
        for i in 0..nsec {
            let d = self.read_at((vds_loc + i) * SECTOR, 2048)?;
            match u16le(&d, 0) {
                1 => {
                    // Primary Volume Descriptor: volume identifier (d-string, 32 bytes at 24).
                    label = dstring(&d[24..56]);
                }
                5 => parts.push((u16le(&d, 22), u32le(&d, 188) as u64)),
                6 => lvd = Some(d),
                8 => break,
                _ => {}
            }
        }
        let l = lvd.ok_or_else(|| bad("no UDF logical volume"))?;
        let block = u32le(&l, 212) as u64;
        if block != SECTOR {
            return Err(NyError::new(ErrorKind::ImageUnsupported, format!("UDF with {block}-byte blocks is not supported.")));
        }
        if label.is_empty() {
            label = dstring(&l[84..212]);
        }
        let nmaps = u32le(&l, 268) as usize;
        let mut maps: Vec<u64> = vec![]; // partition ref -> start sector
        let mut off = 440usize;
        for _ in 0..nmaps {
            let t = l[off];
            let len = l[off + 1] as usize;
            let pnum = match t {
                1 => u16le(&l, off + 4),
                2 => {
                    let id = String::from_utf8_lossy(&l[off + 5..off + 36]).to_string();
                    if id.contains("Metadata") || id.contains("Virtual") {
                        return Err(NyError::new(
                            ErrorKind::ImageUnsupported,
                            "This ISO uses a UDF feature (metadata/virtual partition) that NyForge cannot read yet.",
                        ));
                    }
                    u16le(&l, off + 38) // sparable: read like a plain partition
                }
                _ => return Err(bad("unknown UDF partition map")),
            };
            let start = parts.iter().find(|p| p.0 == pnum).ok_or_else(|| bad("UDF partition missing"))?.1;
            maps.push(start);
            off += len.max(2);
        }
        // File Set Descriptor: where, per the logical volume's contents-use long_ad.
        let fsd_block = u32le(&l, 252) as u64;
        let fsd_ref = u16le(&l, 256) as usize;
        let base = *maps.get(fsd_ref).ok_or_else(|| bad("bad partition reference"))?;
        let fsd = self.read_at((base + fsd_block) * SECTOR, 2048)?;
        if u16le(&fsd, 0) != 256 {
            return Err(bad("no file set descriptor"));
        }
        // RootDirectoryICB: long_ad at 400.
        let root_block = u32le(&fsd, 404) as u64;
        let root_ref = u16le(&fsd, 408) as usize;
        let u = Udf { maps, label, root: (root_ref, root_block) };
        Ok(u)
    }

    /// Resolve a UDF ICB into content.
    fn udf_content(&self, u: &Udf, part_ref: usize, block: u64) -> Result<(Content, bool, u64)> {
        let base = *u.maps.get(part_ref).ok_or_else(|| bad("bad partition reference"))?;
        let d = self.read_at((base + block) * SECTOR, 2048)?;
        let tag = u16le(&d, 0);
        if tag != 261 && tag != 266 {
            return Err(bad("expected a UDF file entry"));
        }
        let ftype = d[16 + 11];
        let flags = u16le(&d, 16 + 18);
        let size = u64le(&d, 56);
        let (l_ea, l_ad, ad_start) = if tag == 261 {
            (u32le(&d, 168) as usize, u32le(&d, 172) as usize, 176usize)
        } else {
            (u32le(&d, 208) as usize, u32le(&d, 212) as usize, 216usize)
        };
        let ad_off = ad_start + l_ea;
        let ads = d.get(ad_off..ad_off + l_ad).ok_or_else(|| bad("allocation descriptors out of range"))?;
        let is_dir = ftype == 4;
        match flags & 7 {
            3 => Ok((Content::Inline(ads.to_vec()), is_dir, size)),
            0 | 1 => {
                let step = if flags & 7 == 0 { 8 } else { 16 };
                let mut exts: Vec<(u64, u64)> = vec![];
                let mut cur: Vec<u8> = ads.to_vec();
                let mut i = 0;
                loop {
                    if i + step > cur.len() {
                        break;
                    }
                    let raw_len = u32le(&cur, i) as u64;
                    let kind = raw_len >> 30;
                    let len = raw_len & 0x3FFF_FFFF;
                    if len == 0 {
                        break;
                    }
                    let blk = u32le(&cur, i + 4) as u64;
                    let pref = if step == 16 { u16le(&cur, i + 8) as usize } else { part_ref };
                    let b = *u.maps.get(pref).ok_or_else(|| bad("bad partition reference"))?;
                    match kind {
                        0 => exts.push(((b + blk) * SECTOR, len)),
                        1 | 2 => exts.push((u64::MAX, len)), // hole: reads as zeros
                        _ => {
                            // Continuation: another block full of descriptors.
                            let more = self.read_at((b + blk) * SECTOR, len.min(2048) as usize)?;
                            cur = more[24..].to_vec();
                            i = 0;
                            continue;
                        }
                    }
                    i += step;
                }
                // The final extent may be longer than the file.
                let mut left = size;
                for e in exts.iter_mut() {
                    e.1 = e.1.min(left);
                    left -= e.1;
                }
                Ok((Content::Extents(exts), is_dir, size))
            }
            _ => Err(NyError::new(ErrorKind::ImageUnsupported, "Unsupported UDF allocation type.")),
        }
    }

    fn udf_list(&self, u: &Udf, dir: &Content) -> Result<Vec<Entry>> {
        let raw = self.content_bytes(dir, u64::MAX)?;
        let mut out = vec![];
        let mut pos = 0usize;
        while pos + 38 <= raw.len() {
            if u16le(&raw, pos) != 257 {
                break;
            }
            let chars = raw[pos + 18];
            let idlen = raw[pos + 19] as usize;
            let iuse = u16le(&raw, pos + 36) as usize;
            let total = (38 + iuse + idlen + 3) & !3;
            let icb_block = u32le(&raw, pos + 24) as u64;
            let icb_ref = u16le(&raw, pos + 28) as usize;
            let id_start = pos + 38 + iuse;
            let id = raw.get(id_start..id_start + idlen).unwrap_or(&[]);
            pos += total;
            if chars & 0x08 != 0 || chars & 0x04 != 0 || idlen == 0 {
                continue; // parent or deleted
            }
            let name = match id[0] {
                8 => id[1..].iter().map(|&b| b as char).collect::<String>(),
                16 => ucs2be(&id[1..]),
                _ => continue,
            };
            let (content, is_dir, size) = self.udf_content(u, icb_ref, icb_block)?;
            out.push(Entry { name, is_dir, size, content });
        }
        Ok(out)
    }

    // ----------------------------------------------------------- public API

    fn content_bytes(&self, c: &Content, limit: u64) -> Result<Vec<u8>> {
        match c {
            Content::Inline(v) => Ok(v.clone()),
            Content::Extents(exts) => {
                let mut v = vec![];
                for (off, len) in exts {
                    if v.len() as u64 + len > limit.min(1 << 28) {
                        return Err(bad("a directory is implausibly large"));
                    }
                    if *off == u64::MAX {
                        v.resize(v.len() + *len as usize, 0);
                    } else {
                        v.extend(self.read_at(*off, *len as usize)?);
                    }
                }
                Ok(v)
            }
        }
    }

    fn root(&self) -> Result<(Content, bool)> {
        match &self.backend {
            Backend::Iso { root, joliet } => Ok((root.clone(), *joliet)),
            Backend::Udf(u) => {
                let (c, _, _) = self.udf_content(u, u.root.0, u.root.1)?;
                Ok((c, false))
            }
        }
    }

    fn list_content(&self, c: &Content, joliet: bool) -> Result<Vec<Entry>> {
        match &self.backend {
            Backend::Iso { .. } => self.iso_list(c, joliet),
            Backend::Udf(u) => self.udf_list(u, c),
        }
    }

    /// List a folder (`"/"`, `"/sources"`, `"boot/grub"`). Names compare case-insensitively.
    pub fn list(&self, path: &str) -> Result<Vec<Entry>> {
        let (mut cur, joliet) = self.root()?;
        for part in path.split('/').filter(|p| !p.is_empty()) {
            let entries = self.list_content(&cur, joliet)?;
            let e = entries
                .into_iter()
                .find(|e| e.name.eq_ignore_ascii_case(part) && e.is_dir)
                .ok_or_else(|| NyError::new(ErrorKind::InvalidInput, format!("No folder \"{part}\" in the ISO.")))?;
            cur = e.content;
        }
        self.list_content(&cur, joliet)
    }

    pub fn find(&self, path: &str) -> Result<Option<Entry>> {
        let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        let Some((name, dirs)) = parts.split_last() else { return Ok(None) };
        let list = match self.list(&dirs.join("/")) {
            Ok(l) => l,
            Err(e) if e.kind == ErrorKind::InvalidInput => return Ok(None),
            Err(e) => return Err(e),
        };
        Ok(list.into_iter().find(|e| e.name.eq_ignore_ascii_case(name)))
    }

    pub fn exists(&self, path: &str) -> bool {
        matches!(self.find(path), Ok(Some(_)))
    }

    /// Read part of a file. Returns the number of bytes read (0 at the end).
    pub fn read(&self, e: &Entry, offset: u64, buf: &mut [u8]) -> Result<usize> {
        if offset >= e.size {
            return Ok(0);
        }
        let want = (buf.len() as u64).min(e.size - offset) as usize;
        match &e.content {
            Content::Inline(v) => {
                let s = offset as usize;
                let n = want.min(v.len().saturating_sub(s));
                buf[..n].copy_from_slice(&v[s..s + n]);
                Ok(n)
            }
            Content::Extents(exts) => {
                let mut done = 0usize;
                let mut pos = 0u64;
                for (off, len) in exts {
                    let end = pos + len;
                    let cur = offset + done as u64;
                    if cur >= end {
                        pos = end;
                        continue;
                    }
                    let skip = cur - pos;
                    let n = ((len - skip) as usize).min(want - done);
                    if *off == u64::MAX {
                        buf[done..done + n].fill(0);
                    } else {
                        read_exact_at(&self.file, &mut buf[done..done + n], off + skip).map_err(|e| NyError::from_io(&e, "reading the ISO"))?;
                    }
                    done += n;
                    pos = end;
                    if done == want {
                        break;
                    }
                }
                Ok(done)
            }
        }
    }

    pub fn read_all(&self, e: &Entry, limit: u64) -> Result<Vec<u8>> {
        if e.size > limit {
            return Err(NyError::invalid("That file inside the ISO is too large to read into memory."));
        }
        let mut v = vec![0u8; e.size as usize];
        let n = self.read(e, 0, &mut v)?;
        v.truncate(n);
        Ok(v)
    }

    /// Depth-first list of every file path (no directories), capped.
    pub fn walk(&self, max_entries: usize) -> Result<Vec<(String, u64)>> {
        let mut out = vec![];
        let mut stack = vec![String::new()];
        while let Some(dir) = stack.pop() {
            for e in self.list(&dir)? {
                let p = if dir.is_empty() { e.name.clone() } else { format!("{dir}/{}", e.name) };
                if e.is_dir {
                    stack.push(p);
                } else {
                    out.push((p, e.size));
                }
                if out.len() >= max_entries {
                    return Ok(out);
                }
            }
        }
        Ok(out)
    }
}

struct Udf {
    maps: Vec<u64>,
    label: String,
    root: (usize, u64),
}

fn dstring(b: &[u8]) -> String {
    // UDF d-string: first byte is the compression id; last byte is the used length.
    if b.is_empty() || b[0] == 0 {
        return String::new();
    }
    let used = (*b.last().unwrap() as usize).min(b.len() - 1);
    let body = &b[1..used.max(1).min(b.len())];
    match b[0] {
        8 => body.iter().map(|&c| c as char).collect::<String>().trim().to_string(),
        16 => ucs2be(body).trim().to_string(),
        _ => String::new(),
    }
}

/// Rock Ridge "NM" records inside a directory record's system-use area.
fn rock_ridge_name(su: &[u8]) -> Option<String> {
    let mut i = 0;
    let mut name = Vec::new();
    let mut found = false;
    while i + 4 <= su.len() {
        let sig = &su[i..i + 2];
        let len = su[i + 2] as usize;
        if len < 4 || i + len > su.len() {
            break;
        }
        if sig == b"NM" && len >= 5 {
            let flags = su[i + 4];
            if flags & 0x06 == 0 {
                name.extend_from_slice(&su[i + 5..i + len]);
                found = true;
            }
        }
        i += len;
    }
    found.then(|| String::from_utf8_lossy(&name).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn have(cmd: &str) -> bool {
        Command::new("sh").args(["-c", &format!("command -v {cmd} >/dev/null")]).status().map(|s| s.success()).unwrap_or(false)
    }

    fn tree(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("nyforge-iso-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("t/sources")).unwrap();
        std::fs::create_dir_all(d.join("t/boot/grub")).unwrap();
        std::fs::write(d.join("t/bootmgr"), b"BM").unwrap();
        std::fs::write(d.join("t/sources/boot.wim"), vec![7u8; 100_000]).unwrap();
        std::fs::write(d.join("t/sources/Install Image With Long Name.esd"), vec![9u8; 5_000]).unwrap();
        std::fs::write(d.join("t/boot/grub/loopback.cfg"), b"menuentry x {}\n").unwrap();
        std::fs::write(d.join("t/readme.txt"), b"hello iso").unwrap();
        d
    }

    fn check_tree(iso: &IsoImage) {
        let root: Vec<String> = iso.list("/").unwrap().into_iter().map(|e| e.name.to_lowercase()).collect();
        assert!(root.contains(&"bootmgr".to_string()), "{root:?}");
        assert!(root.contains(&"sources".to_string()), "{root:?}");
        let w = iso.find("/Sources/BOOT.WIM").unwrap().expect("case-insensitive lookup");
        assert_eq!(w.size, 100_000);
        assert_eq!(iso.read_all(&w, 1 << 20).unwrap(), vec![7u8; 100_000]);
        assert!(iso.exists("boot/grub/loopback.cfg"));
        assert!(!iso.exists("boot/grub/nothing.cfg"));
        let r = iso.find("readme.txt").unwrap().unwrap();
        assert_eq!(iso.read_all(&r, 100).unwrap(), b"hello iso");
        let mut part = [0u8; 4];
        iso.read(&w, 99_998, &mut part).unwrap();
        assert_eq!(&part[..2], &[7, 7], "reads stop at the end of the file");
        let all: Vec<String> = iso.walk(100).unwrap().into_iter().map(|p| p.0).collect();
        assert_eq!(all.len(), 5, "{all:?}");
    }

    #[test]
    fn iso9660_with_rock_ridge_and_joliet() {
        if !have("xorriso") {
            eprintln!("(xorriso missing: skipped)");
            return;
        }
        let d = tree("rr");
        let out = d.join("rr.iso");
        let st = Command::new("xorriso").args(["-as", "mkisofs", "-r", "-J", "-V", "MY LABEL", "-o"]).arg(&out).arg(d.join("t")).output().unwrap();
        assert!(st.status.success());
        let iso = IsoImage::open(&out).unwrap();
        assert_eq!(iso.kind, Kind::Iso9660);
        assert_eq!(iso.label, "MY LABEL");
        check_tree(&iso);
        // Rock Ridge only (no Joliet) must also give the long, mixed-case name.
        let out2 = d.join("rr-only.iso");
        Command::new("xorriso").args(["-as", "mkisofs", "-r", "-V", "X", "-o"]).arg(&out2).arg(d.join("t")).output().unwrap();
        let iso2 = IsoImage::open(&out2).unwrap();
        assert!(iso2.exists("sources/Install Image With Long Name.esd"));
    }

    #[test]
    fn plain_iso9660_strips_version_suffixes() {
        if !have("genisoimage") {
            eprintln!("(genisoimage missing: skipped)");
            return;
        }
        let d = tree("plain");
        let out = d.join("plain.iso");
        let st = Command::new("genisoimage").args(["-iso-level", "1", "-V", "PLAIN", "-o"]).arg(&out).arg(d.join("t")).output().unwrap();
        assert!(st.status.success(), "{}", String::from_utf8_lossy(&st.stderr));
        let iso = IsoImage::open(&out).unwrap();
        let names: Vec<String> = iso.list("/").unwrap().into_iter().map(|e| e.name).collect();
        assert!(names.contains(&"BOOTMGR".to_string()) || names.iter().any(|n| n.starts_with("BOOTMGR")), "{names:?}");
        assert!(names.iter().all(|n| !n.contains(';')), "{names:?}");
    }

    #[test]
    fn udf_hybrid_like_a_windows_installer() {
        if !have("genisoimage") {
            eprintln!("(genisoimage missing: skipped)");
            return;
        }
        let d = tree("udf");
        let out = d.join("udf.iso");
        let st = Command::new("genisoimage")
            .args(["-udf", "-iso-level", "1", "-V", "CCCOMA_X64FRE", "-o"])
            .arg(&out)
            .arg(d.join("t"))
            .output()
            .unwrap();
        assert!(st.status.success(), "{}", String::from_utf8_lossy(&st.stderr));
        let iso = IsoImage::open(&out).unwrap();
        assert_eq!(iso.kind, Kind::Hybrid);
        assert!(iso.label.starts_with("CCCOMA"), "{}", iso.label);
        // The UDF tree has the real long names.
        assert!(iso.exists("sources/Install Image With Long Name.esd"));
        check_tree(&iso);
    }

    #[test]
    fn garbage_is_not_an_iso() {
        let p = std::env::temp_dir().join(format!("nyforge-iso-garbage-{}", std::process::id()));
        std::fs::write(&p, vec![0u8; 100_000]).unwrap();
        assert!(IsoImage::open(&p).is_err());
    }
}
