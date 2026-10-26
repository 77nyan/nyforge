//! Windows Imaging (WIM) files: just enough to split `install.wim` into
//! `install.swm`, `install2.swm`, … so it fits on a FAT32 stick (4 GiB file limit).
//!
//! Compressed data is copied byte for byte, never decompressed, so this runs at
//! disk speed. The layout follows what `wimlib-imagex split` produces, and the
//! tests check our output by applying it with wimlib.
//!
//! Not supported (reported as such, never silently mangled): solid/ESD
//! resources, WIMs that are already split, damaged files.

use crate::error::{ErrorKind, NyError, Result};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

pub const HEADER_LEN: usize = 208;
const MAGIC: &[u8; 8] = b"MSWIM\0\0\0";
const ENTRY_LEN: usize = 50;
const FLAG_METADATA: u8 = 0x02;
const FLAG_SOLID: u8 = 0x10;
const HDR_SPANNED: u32 = 0x8;

/// Largest FAT32 file is 4 GiB - 1; stay clear of it.
pub const FAT32_PART_BYTES: u64 = 3800 * 1024 * 1024;

pub trait WimSource {
    fn len(&self) -> u64;
    fn read_at(&self, buf: &mut [u8], offset: u64) -> Result<()>;
}

impl WimSource for std::fs::File {
    fn len(&self) -> u64 {
        self.metadata().map(|m| m.len()).unwrap_or(0)
    }
    fn read_at(&self, buf: &mut [u8], offset: u64) -> Result<()> {
        crate::sys::read_exact_at(self, buf, offset).map_err(|e| NyError::from_io(&e, "reading the Windows image"))
    }
}

fn bad(msg: &str) -> NyError {
    NyError::new(ErrorKind::ImageCorrupt, format!("The Windows image (WIM) is damaged: {msg}"))
}

#[derive(Debug, Clone, Copy)]
struct ResHdr {
    size: u64,
    flags: u8,
    offset: u64,
    orig: u64,
}

impl ResHdr {
    fn parse(b: &[u8]) -> Self {
        let v = u64::from_le_bytes(b[0..8].try_into().unwrap());
        ResHdr {
            size: v & 0x00FF_FFFF_FFFF_FFFF,
            flags: (v >> 56) as u8,
            offset: u64::from_le_bytes(b[8..16].try_into().unwrap()),
            orig: u64::from_le_bytes(b[16..24].try_into().unwrap()),
        }
    }
    fn write(&self, out: &mut [u8]) {
        out[0..8].copy_from_slice(&(self.size | (self.flags as u64) << 56).to_le_bytes());
        out[8..16].copy_from_slice(&self.offset.to_le_bytes());
        out[16..24].copy_from_slice(&self.orig.to_le_bytes());
    }
    fn is_zero(&self) -> bool {
        self.size == 0 && self.offset == 0 && self.orig == 0
    }
}

#[derive(Debug, Clone)]
struct Blob {
    hdr: ResHdr,
    refcnt: u32,
    hash: [u8; 20],
}

#[derive(Debug, Clone)]
struct Part {
    /// Indices into `streams` (data blobs stored in this part).
    streams: Vec<usize>,
    /// Total bytes of the finished file.
    total: u64,
}

pub struct SplitPlan {
    header: [u8; HEADER_LEN],
    metadata: Vec<Blob>,
    streams: Vec<Blob>,
    xml: Vec<u8>,
    boot_meta_index: Option<usize>,
    parts: Vec<Part>,
    guid: [u8; 16],
}

impl SplitPlan {
    /// Read the WIM's tables and decide which resources go in which part.
    pub fn new(src: &dyn WimSource, max_part: u64) -> Result<Self> {
        let total = src.len();
        if total < HEADER_LEN as u64 {
            return Err(bad("the file is too short"));
        }
        let mut h = [0u8; HEADER_LEN];
        src.read_at(&mut h, 0)?;
        if &h[0..8] != MAGIC {
            return Err(NyError::new(ErrorKind::ImageUnsupported, "This is not a Windows image (WIM) file."));
        }
        let version = u32::from_le_bytes(h[12..16].try_into().unwrap());
        let flags = u32::from_le_bytes(h[16..20].try_into().unwrap());
        let part = u16::from_le_bytes(h[40..42].try_into().unwrap());
        let parts_total = u16::from_le_bytes(h[42..44].try_into().unwrap());
        if version != 0x10d00 {
            return Err(NyError::new(
                ErrorKind::ImageUnsupported,
                "This Windows image uses the compact (solid/ESD) format, which cannot be split.",
            ));
        }
        if parts_total != 1 || part != 1 || flags & HDR_SPANNED != 0 {
            return Err(NyError::new(ErrorKind::ImageUnsupported, "This Windows image is already split."));
        }
        if flags & 0x40 != 0 {
            return Err(bad("it was not finished writing"));
        }
        let lookup = ResHdr::parse(&h[48..72]);
        let xml_h = ResHdr::parse(&h[72..96]);
        let boot = ResHdr::parse(&h[96..120]);
        if lookup.offset + lookup.size > total || xml_h.offset + xml_h.size > total {
            return Err(bad("its tables lie beyond the end of the file"));
        }
        if lookup.size % ENTRY_LEN as u64 != 0 || lookup.flags & 0x04 != 0 {
            return Err(bad("the file table is unreadable"));
        }
        let mut tbl = vec![0u8; lookup.size as usize];
        src.read_at(&mut tbl, lookup.offset)?;
        let mut metadata = vec![];
        let mut streams = vec![];
        for e in tbl.chunks_exact(ENTRY_LEN) {
            let hdr = ResHdr::parse(&e[0..24]);
            let refcnt = u32::from_le_bytes(e[26..30].try_into().unwrap());
            let mut hash = [0u8; 20];
            hash.copy_from_slice(&e[30..50]);
            if hdr.flags & FLAG_SOLID != 0 {
                return Err(NyError::new(ErrorKind::ImageUnsupported, "This Windows image uses the compact (solid/ESD) format, which cannot be split."));
            }
            if hdr.offset.checked_add(hdr.size).map_or(true, |end| end > total) {
                return Err(bad("a file entry lies beyond the end of the file"));
            }
            let b = Blob { hdr, refcnt, hash };
            if hdr.flags & FLAG_METADATA != 0 {
                metadata.push(b);
            } else {
                streams.push(b);
            }
        }
        streams.sort_by_key(|s| s.hdr.offset);
        let mut xml = vec![0u8; xml_h.size as usize];
        src.read_at(&mut xml, xml_h.offset)?;
        let boot_meta_index = if boot.is_zero() { None } else { metadata.iter().position(|m| m.hdr.offset == boot.offset) };

        // Part 1 carries the metadata; every part carries a lookup table and the XML.
        let meta_bytes: u64 = metadata.iter().map(|m| m.hdr.size).sum();
        let mut parts: Vec<Part> = vec![];
        let mut cur = Part { streams: vec![], total: 0 };
        let overhead = |entries: usize| HEADER_LEN as u64 + (entries * ENTRY_LEN) as u64 + xml.len() as u64;
        let mut cur_entries = metadata.len();
        let mut cur_bytes = meta_bytes;
        if overhead(cur_entries) + cur_bytes > max_part {
            return Err(NyError::new(ErrorKind::ImageUnsupported, "The image's index data alone is bigger than one part."));
        }
        for (i, s) in streams.iter().enumerate() {
            if overhead(1) + s.hdr.size > max_part {
                return Err(NyError::new(
                    ErrorKind::ImageUnsupported,
                    "A single file inside this Windows image is too large for a FAT32 stick.",
                ));
            }
            if overhead(cur_entries + 1) + cur_bytes + s.hdr.size > max_part {
                cur.total = overhead(cur_entries) + cur_bytes;
                parts.push(std::mem::replace(&mut cur, Part { streams: vec![], total: 0 }));
                cur_entries = 0;
                cur_bytes = 0;
            }
            cur.streams.push(i);
            cur_entries += 1;
            cur_bytes += s.hdr.size;
        }
        cur.total = overhead(cur_entries) + cur_bytes;
        parts.push(cur);

        let mut guid = [0u8; 16];
        getrandom::fill(&mut guid).map_err(|e| NyError::other(format!("no random numbers available: {e}")))?;
        Ok(SplitPlan { header: h, metadata, streams, xml, boot_meta_index, parts, guid })
    }

    /// Use this GUID instead of a fresh random one, to rebuild parts byte for byte as an earlier run wrote them.
    pub fn with_guid(mut self, guid: [u8; 16]) -> Self {
        self.guid = guid;
        self
    }

    /// The GUID that ties the parts of one split image together, read from the first bytes of any part.
    pub fn guid_of_part(header: &[u8]) -> Option<[u8; 16]> {
        if header.len() < HEADER_LEN || &header[0..8] != MAGIC {
            return None;
        }
        header[24..40].try_into().ok()
    }

    pub fn part_count(&self) -> usize {
        self.parts.len()
    }
    pub fn part_size(&self, i: usize) -> u64 {
        self.parts[i].total
    }
    pub fn total_bytes(&self) -> u64 {
        self.parts.iter().map(|p| p.total).sum()
    }

    /// File name for part `i` (0-based): `install.swm`, `install2.swm`, …
    pub fn part_name(base: &str, i: usize) -> String {
        if i == 0 {
            format!("{base}.swm")
        } else {
            format!("{base}{}.swm", i + 1)
        }
    }

    /// Write part `i` to `out` (sequentially, no seeking).
    pub fn write_part(
        &self,
        i: usize,
        src: &dyn WimSource,
        out: &mut dyn Write,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(u64),
    ) -> Result<()> {
        let io = |e: std::io::Error| NyError::from_io(&e, "writing the split Windows image");
        let part = &self.parts[i];
        let first = i == 0;

        // Layout: header | streams | (metadata) | lookup | xml
        let mut off = HEADER_LEN as u64;
        let mut lookup: Vec<u8> = vec![];
        let mut stream_pos: Vec<u64> = vec![];
        for &si in &part.streams {
            stream_pos.push(off);
            off += self.streams[si].hdr.size;
        }
        let mut meta_pos: Vec<u64> = vec![];
        if first {
            for m in &self.metadata {
                meta_pos.push(off);
                off += m.hdr.size;
            }
        }
        let entry = |b: &Blob, at: u64, partno: u16, out: &mut Vec<u8>| {
            let mut e = [0u8; ENTRY_LEN];
            ResHdr { offset: at, ..b.hdr }.write(&mut e[0..24]);
            e[24..26].copy_from_slice(&partno.to_le_bytes());
            e[26..30].copy_from_slice(&b.refcnt.to_le_bytes());
            e[30..50].copy_from_slice(&b.hash);
            out.extend_from_slice(&e);
        };
        if first {
            for (m, &p) in self.metadata.iter().zip(&meta_pos) {
                entry(m, p, 1, &mut lookup);
            }
        }
        for (&si, &p) in part.streams.iter().zip(&stream_pos) {
            entry(&self.streams[si], p, (i + 1) as u16, &mut lookup);
        }
        let lookup_off = off;
        let xml_off = lookup_off + lookup.len() as u64;

        // Header.
        let mut h = self.header;
        let flags = u32::from_le_bytes(h[16..20].try_into().unwrap()) | HDR_SPANNED;
        h[16..20].copy_from_slice(&flags.to_le_bytes());
        h[24..40].copy_from_slice(&self.guid);
        h[40..42].copy_from_slice(&((i + 1) as u16).to_le_bytes());
        h[42..44].copy_from_slice(&(self.parts.len() as u16).to_le_bytes());
        ResHdr { size: lookup.len() as u64, flags: FLAG_METADATA, offset: lookup_off, orig: lookup.len() as u64 }.write(&mut h[48..72]);
        ResHdr { size: self.xml.len() as u64, flags: FLAG_METADATA, offset: xml_off, orig: self.xml.len() as u64 }.write(&mut h[72..96]);
        let boot_hdr = match (first, self.boot_meta_index) {
            (true, Some(bi)) => {
                let m = &self.metadata[bi];
                ResHdr { offset: meta_pos[bi], ..m.hdr }
            }
            _ => ResHdr { size: 0, flags: 0, offset: 0, orig: 0 },
        };
        boot_hdr.write(&mut h[96..120]);
        if boot_hdr.is_zero() {
            h[120..124].copy_from_slice(&0u32.to_le_bytes());
        }
        h[124..148].fill(0); // no integrity table
        out.write_all(&h).map_err(io)?;

        // Data.
        let mut buf = vec![0u8; 1 << 20];
        let mut copy = |b: &Blob, out: &mut dyn Write, progress: &mut dyn FnMut(u64)| -> Result<()> {
            let mut left = b.hdr.size;
            let mut at = b.hdr.offset;
            while left > 0 {
                if cancel.load(Ordering::Relaxed) {
                    return Err(NyError::cancelled());
                }
                let n = left.min(buf.len() as u64) as usize;
                src.read_at(&mut buf[..n], at)?;
                out.write_all(&buf[..n]).map_err(io)?;
                at += n as u64;
                left -= n as u64;
                progress(n as u64);
            }
            Ok(())
        };
        for &si in &part.streams {
            copy(&self.streams[si], out, progress)?;
        }
        if first {
            for m in &self.metadata {
                copy(m, out, progress)?;
            }
        }
        out.write_all(&lookup).map_err(io)?;
        out.write_all(&self.xml).map_err(io)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn have_wimlib() -> bool {
        Command::new("sh").args(["-c", "command -v wimlib-imagex >/dev/null"]).status().map(|s| s.success()).unwrap_or(false)
    }

    fn sample_tree(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nyforge-wim-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("src/a")).unwrap();
        std::fs::create_dir_all(d.join("src/b")).unwrap();
        let mut x = 12345u64;
        for i in 1..=6usize {
            let data: Vec<u8> = (0..i * 300_000)
                .map(|_| {
                    x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                    (x >> 33) as u8
                })
                .collect();
            std::fs::write(d.join(format!("src/a/f{i}.bin")), data).unwrap();
        }
        std::fs::write(d.join("src/b/text.txt"), "compressible text line\n".repeat(90_000)).unwrap();
        std::fs::write(d.join("src/small.txt"), "hi\n").unwrap();
        d
    }

    fn split_to(wim: &Path, outdir: &Path, max: u64) -> usize {
        std::fs::create_dir_all(outdir).unwrap();
        let f = std::fs::File::open(wim).unwrap();
        let plan = SplitPlan::new(&f, max).unwrap();
        let cancel = AtomicBool::new(false);
        for i in 0..plan.part_count() {
            let mut o = std::fs::File::create(outdir.join(SplitPlan::part_name("install", i))).unwrap();
            plan.write_part(i, &f, &mut o, &cancel, &mut |_| {}).unwrap();
            assert_eq!(o.metadata().unwrap().len(), plan.part_size(i), "planned size of part {i}");
            assert!(plan.part_size(i) <= max);
        }
        plan.part_count()
    }

    fn assert_same_tree(a: &Path, b: &Path) {
        let st = Command::new("diff").args(["-r"]).arg(a).arg(b).output().unwrap();
        assert!(st.status.success(), "trees differ: {}", String::from_utf8_lossy(&st.stdout));
    }

    fn roundtrip(compress: &str, max: u64) {
        let d = sample_tree(&format!("rt-{compress}-{max}"));
        let wim = d.join("a.wim");
        let st = Command::new("wimlib-imagex")
            .args(["capture"])
            .arg(d.join("src"))
            .arg(&wim)
            .args(["Test", &format!("--compress={compress}")])
            .output()
            .unwrap();
        assert!(st.status.success(), "{}", String::from_utf8_lossy(&st.stderr));
        let parts = split_to(&wim, &d.join("out"), max);
        assert!(parts > 1, "test should produce several parts");
        // wimlib must accept the set and extract identical files.
        let info = Command::new("wimlib-imagex").arg("info").arg(d.join("out/install.swm")).output().unwrap();
        assert!(info.status.success(), "{}", String::from_utf8_lossy(&info.stderr));
        let st = Command::new("wimlib-imagex")
            .arg("apply")
            .arg(d.join("out/install.swm"))
            .arg("1")
            .arg(d.join("applied"))
            .arg(format!("--ref={}", d.join("out/install*.swm").display()))
            .output()
            .unwrap();
        assert!(st.status.success(), "{}", String::from_utf8_lossy(&st.stderr));
        assert_same_tree(&d.join("src"), &d.join("applied"));
        let v = Command::new("wimlib-imagex")
            .arg("verify")
            .arg(d.join("out/install.swm"))
            .arg(format!("--ref={}", d.join("out/install*.swm").display()))
            .output()
            .unwrap();
        assert!(v.status.success(), "{}", String::from_utf8_lossy(&v.stderr));
    }

    #[test]
    fn split_uncompressed_wim_is_accepted_by_wimlib() {
        if !have_wimlib() {
            return;
        }
        roundtrip("none", 2_200_000);
    }

    #[test]
    fn split_lzx_and_xpress_wim_is_accepted_by_wimlib() {
        if !have_wimlib() {
            return;
        }
        roundtrip("LZX", 2_200_000);
        roundtrip("XPRESS", 2_200_000);
    }

    #[test]
    fn an_unsplit_fit_gives_one_part() {
        if !have_wimlib() {
            return;
        }
        let d = sample_tree("one");
        let wim = d.join("a.wim");
        Command::new("wimlib-imagex").arg("capture").arg(d.join("src")).arg(&wim).args(["T", "--compress=none"]).output().unwrap();
        assert_eq!(split_to(&wim, &d.join("out"), 100 << 20), 1);
        let st = Command::new("wimlib-imagex").arg("apply").arg(d.join("out/install.swm")).arg("1").arg(d.join("applied")).output().unwrap();
        assert!(st.status.success(), "{}", String::from_utf8_lossy(&st.stderr));
        assert_same_tree(&d.join("src"), &d.join("applied"));
    }

    #[test]
    fn rejects_non_wim_and_split_input() {
        let p = std::env::temp_dir().join(format!("nyforge-wim-bad-{}", std::process::id()));
        std::fs::write(&p, vec![0u8; 4096]).unwrap();
        let f = std::fs::File::open(&p).unwrap();
        assert_eq!(SplitPlan::new(&f, 1 << 30).err().unwrap().kind, ErrorKind::ImageUnsupported);
        if have_wimlib() {
            let d = sample_tree("again");
            let wim = d.join("a.wim");
            Command::new("wimlib-imagex").arg("capture").arg(d.join("src")).arg(&wim).args(["T", "--compress=none"]).output().unwrap();
            split_to(&wim, &d.join("out"), 2_200_000);
            let again = std::fs::File::open(d.join("out/install.swm")).unwrap();
            let e = SplitPlan::new(&again, 1 << 30).err().unwrap();
            assert!(e.message.contains("already split"), "{}", e.message);
        }
    }

    #[test]
    fn part_names_follow_windows_convention() {
        assert_eq!(SplitPlan::part_name("install", 0), "install.swm");
        assert_eq!(SplitPlan::part_name("install", 1), "install2.swm");
        assert_eq!(SplitPlan::part_name("install", 9), "install10.swm");
    }

    #[test]
    fn truncated_wim_is_reported_not_trusted() {
        if !have_wimlib() {
            return;
        }
        let d = sample_tree("trunc");
        let wim = d.join("a.wim");
        Command::new("wimlib-imagex").arg("capture").arg(d.join("src")).arg(&wim).args(["T", "--compress=none"]).output().unwrap();
        let data = std::fs::read(&wim).unwrap();
        let cut = d.join("cut.wim");
        std::fs::write(&cut, &data[..data.len() / 2]).unwrap();
        let f = std::fs::File::open(&cut).unwrap();
        let e = SplitPlan::new(&f, 1 << 30).err().unwrap();
        assert_eq!(e.kind, ErrorKind::ImageCorrupt);
    }
}
