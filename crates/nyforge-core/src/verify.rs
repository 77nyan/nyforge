//! Read a drive back and compare it with the image that was written to it.
//!
//! The image is decoded again (so a `.xz` or `.zip` is unpacked a second time) and every byte is compared with what the
//! drive returns. Reads from a drive bypass the operating system's cache, so what is compared is what the flash chips
//! really hold, not what is still sitting in memory. This is what tells a good write from a failing or counterfeit
//! stick that quietly dropped data.
//!
//! Nothing is ever written to the drive here.

use crate::alignedbuf::{AlignedBuf, ALIGN};
use crate::error::{ErrorKind, NyError, Result};
use crate::format::{human_size, round_up};
use crate::image::ImageSource;
use crate::progress::*;
use crate::sys::{self, read_exact_at, TargetKind};
use crate::writer::{map_read_error, Target};
use crossbeam_channel::{bounded, unbounded, RecvTimeoutError};
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct VerifyOptions {
    pub chunk_size: usize,
    pub direct_io: bool,
    pub progress_interval: Duration,
}

impl Default for VerifyOptions {
    fn default() -> Self {
        Self { chunk_size: 4 << 20, direct_io: true, progress_interval: Duration::from_millis(120) }
    }
}

#[derive(Debug, Clone)]
pub struct VerifyReport {
    pub bytes: u64,
    pub elapsed: Duration,
    /// Set when the drive matches the image except for small differences inside parts of the image that other systems
    /// are known to rewrite once the drive is plugged in (an embedded EFI/FAT or BSD partition, the partition table's
    /// disk ID). Says what and where; `None` for an exact match.
    pub note: Option<String>,
}

// ------------------------------------------------------------------ parts of an image that get rewritten

/// What kind of part of an image another system may rewrite after the drive is plugged in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum VolatileKind {
    /// An EFI System Partition inside the image.
    Efi,
    /// Another FAT (or Windows data) partition inside the image.
    Fat,
    /// A BSD slice or partition (UFS, ZFS, boot) inside the image.
    Bsd,
    /// The GPT header and entries, at the start and at the end of the image.
    GptTable,
    /// The disk signature in the MBR.
    DiskId,
}

impl VolatileKind {
    fn what(self) -> &'static str {
        match self {
            VolatileKind::Efi => "the image's own EFI boot partition",
            VolatileKind::Fat => "a FAT partition inside the image",
            VolatileKind::Bsd => "a BSD partition inside the image",
            VolatileKind::GptTable => "the GPT partition table",
            VolatileKind::DiskId => "the disk ID in the MBR",
        }
    }
}

/// The most differing bytes forgiven in total. A rewritten boot sector or superblock is a few hundred bytes; a
/// partition that really lost data differs by far more, and that stays a failure.
const FORGIVEN_MAX: u64 = 256 * 1024;

/// Where in an image there is something a system may rewrite once the drive is plugged in, read from the image's own
/// partition table. Hybrid ISOs and disk images of Void Linux and the BSDs carry a small FAT or BSD partition of
/// their own; Windows, the BSDs and firmware tools touch those (volume flags, boot sector fields, a "dirty" mark), so
/// a read-back a moment later can legitimately differ in a few bytes there. Everything outside these ranges has to
/// match exactly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Volatile {
    ranges: Vec<(u64, u64, VolatileKind)>,
}

/// A GPT type GUID as it is stored on disk (the first three groups little-endian).
fn guid_bytes(s: &str) -> [u8; 16] {
    let hex: Vec<u8> = s.bytes().filter(|b| *b != b'-').collect();
    let mut raw = [0u8; 16];
    for i in 0..16 {
        let h = |c: u8| (c as char).to_digit(16).unwrap_or(0) as u8;
        raw[i] = h(hex[i * 2]) << 4 | h(hex[i * 2 + 1]);
    }
    raw[0..4].reverse();
    raw[4..6].reverse();
    raw[6..8].reverse();
    raw
}

fn gpt_kind(guid: &[u8]) -> Option<VolatileKind> {
    const EFI: &str = "C12A7328-F81F-11D2-BA4B-00A0C93EC93B";
    const DATA: &str = "EBD0A0A2-B9E5-4433-87C0-68B6B72699C7";
    const BSD: [&str; 8] = [
        "516E7CB4-6ECF-11D6-8FF8-00022D09712B", // FreeBSD data
        "516E7CB5-6ECF-11D6-8FF8-00022D09712B", // FreeBSD swap
        "516E7CB6-6ECF-11D6-8FF8-00022D09712B", // FreeBSD UFS
        "516E7CB8-6ECF-11D6-8FF8-00022D09712B", // FreeBSD Vinum
        "516E7CBA-6ECF-11D6-8FF8-00022D09712B", // FreeBSD ZFS
        "83BD6B9D-7F41-11DC-BE0B-001560B84F0F", // FreeBSD boot
        "49F48D5A-B10E-11DC-B99B-0019D1879648", // NetBSD FFS
        "824CC7A0-36A8-11E3-890A-952519AD3F61", // OpenBSD data
    ];
    if guid == guid_bytes(EFI) {
        Some(VolatileKind::Efi)
    } else if guid == guid_bytes(DATA) {
        Some(VolatileKind::Fat)
    } else if BSD.iter().any(|g| guid == guid_bytes(g)) {
        Some(VolatileKind::Bsd)
    } else {
        None
    }
}

impl Volatile {
    /// Read the image's partition table from its first bytes (a few KiB are enough). `image_len`, when known, places
    /// the backup GPT at the end of the image.
    pub fn from_head(head: &[u8], image_len: Option<u64>) -> Volatile {
        let mut v = Volatile::default();
        if head.len() < 512 || head[510] != 0x55 || head[511] != 0xAA {
            return v;
        }
        v.ranges.push((440, 446, VolatileKind::DiskId));
        let mut protective = false;
        for i in 0..4usize {
            let e = &head[446 + i * 16..462 + i * 16];
            let ty = e[4];
            let start = u32::from_le_bytes([e[8], e[9], e[10], e[11]]) as u64;
            let len = u32::from_le_bytes([e[12], e[13], e[14], e[15]]) as u64;
            if ty == 0 || len == 0 {
                continue;
            }
            let kind = match ty {
                0xEE => {
                    protective = true;
                    continue;
                }
                0xEF => VolatileKind::Efi,
                0x01 | 0x04 | 0x06 | 0x0B | 0x0C | 0x0E => VolatileKind::Fat,
                0xA5 | 0xA6 | 0xA9 => VolatileKind::Bsd,
                _ => continue,
            };
            v.ranges.push((start * 512, (start + len) * 512, kind));
        }
        if protective || head.len() >= 1024 {
            v.read_gpt(head, image_len);
        }
        v
    }

    fn read_gpt(&mut self, head: &[u8], image_len: Option<u64>) {
        if head.len() < 1024 || &head[512..520] != b"EFI PART" {
            return;
        }
        let h = &head[512..1024];
        let entries_lba = u64::from_le_bytes(h[72..80].try_into().unwrap());
        let count = u32::from_le_bytes(h[80..84].try_into().unwrap()) as u64;
        let esize = u32::from_le_bytes(h[84..88].try_into().unwrap()) as u64;
        if !(128..=4096).contains(&esize) || count == 0 || count > 1024 {
            return;
        }
        let table_end = entries_lba * 512 + count * esize;
        self.ranges.push((512, table_end.max(1024), VolatileKind::GptTable));
        if let Some(len) = image_len {
            if len > 34 * 512 {
                self.ranges.push((len - 33 * 512, len, VolatileKind::GptTable));
            }
        }
        for i in 0..count {
            let at = (entries_lba * 512 + i * esize) as usize;
            let Some(e) = head.get(at..at + 128.min(esize as usize)) else { break };
            if e.len() < 48 || e[0..16].iter().all(|&b| b == 0) {
                continue;
            }
            let first = u64::from_le_bytes(e[32..40].try_into().unwrap());
            let last = u64::from_le_bytes(e[40..48].try_into().unwrap());
            if last < first {
                continue;
            }
            if let Some(kind) = gpt_kind(&e[0..16]) {
                self.ranges.push((first * 512, (last + 1) * 512, kind));
            }
        }
    }

    /// Does this image carry a partition that another system may rewrite (not counting the tables)?
    pub fn has_partitions(&self) -> bool {
        self.ranges.iter().any(|r| matches!(r.2, VolatileKind::Efi | VolatileKind::Fat | VolatileKind::Bsd))
    }

    pub fn has_bsd(&self) -> bool {
        self.ranges.iter().any(|r| r.2 == VolatileKind::Bsd)
    }

    fn covers(&self, pos: u64) -> Option<VolatileKind> {
        // a partition wins over the table that describes it, when both claim a byte
        self.ranges.iter().filter(|r| pos >= r.0 && pos < r.1).map(|r| r.2).min()
    }
}

/// Differences found in parts of the image that other systems rewrite.
#[derive(Debug, Default)]
struct Tally {
    bytes: u64,
    first: Option<u64>,
    kinds: std::collections::BTreeSet<VolatileKind>,
}

impl Tally {
    fn note(&self) -> Option<String> {
        let first = self.first?;
        let what: Vec<&str> = self.kinds.iter().map(|k| k.what()).collect();
        Some(format!(
            "{} byte{} differ inside {} (first at byte {}), a part another system may rewrite once the drive is plugged in. Everything else matches the image exactly.",
            self.bytes,
            if self.bytes == 1 { "" } else { "s" },
            what.join(" and "),
            first
        ))
    }
}

/// Compare one chunk. Differences inside [`Volatile`] ranges are counted and allowed (up to [`FORGIVEN_MAX`] in
/// total); the first difference anywhere else is an error: `Err((index in the chunk, differing bytes in the chunk))`.
fn settle(volatile: &Volatile, offset: u64, image: &[u8], drive: &[u8], tally: &mut Tally) -> std::result::Result<(), (usize, usize)> {
    let total = image.iter().zip(drive).filter(|(x, y)| x != y).count();
    let mut found: Vec<(u64, VolatileKind)> = vec![];
    for (i, (x, y)) in image.iter().zip(drive).enumerate() {
        if x == y {
            continue;
        }
        match volatile.covers(offset + i as u64) {
            Some(k) => found.push((offset + i as u64, k)),
            None => return Err((i, total)),
        }
    }
    if tally.bytes + found.len() as u64 > FORGIVEN_MAX {
        let first = found.first().map_or(0, |f| (f.0 - offset) as usize);
        return Err((first, total));
    }
    for (pos, k) in found {
        tally.first.get_or_insert(pos);
        tally.bytes += 1;
        tally.kinds.insert(k);
    }
    Ok(())
}

/// A heads-up to show before writing, for images known to carry a partition that systems rewrite: the BSDs and
/// Void Linux. `head` is the start of the image (at least 64 KiB when there is that much) and `name` its file name
/// or label. `None` for everything else. Verifying such an image stays exact outside those partitions.
pub fn verify_heads_up(head: &[u8], name: &str, image_len: Option<u64>) -> Option<String> {
    let v = Volatile::from_head(head, image_len);
    if !v.has_partitions() {
        return None;
    }
    let lower = name.to_ascii_lowercase();
    let known = ["void", "bsd", "dragonfly", "pfsense", "opnsense", "truenas", "nomadbsd", "ghostbsd"].iter().any(|k| lower.contains(k));
    if !(v.has_bsd() || known) {
        return None;
    }
    Some("This image carries a small boot partition of its own that Windows and other systems rewrite when the drive is plugged in. \"Verify\" can report a few differing bytes there; NyForge tells those apart from a real failed write, and everything else is still compared exactly.".to_string())
}

/// The image bytes at a position, handed from the thread that decodes the image to the one that compares.
enum Msg {
    Chunk { buf: AlignedBuf, len: usize, offset: u64 },
    Eof,
    Failed(NyError),
}

/// How many bytes to ask the drive for to compare `len` image bytes at `offset`.
///
/// Raw handles (Windows, macOS, and direct I/O on Linux) only take whole sectors; the answer is widened to a sector but
/// never past the end of the drive.
pub fn read_len(len: usize, offset: u64, direct: bool, sector: u64, capacity: Option<u64>) -> usize {
    if !direct {
        return len;
    }
    let mut n = round_up(len as u64, sector.max(1));
    if let Some(cap) = capacity {
        n = n.min(cap.saturating_sub(offset));
    }
    (n as usize).max(len)
}

/// Describe the first place two buffers differ.
fn first_difference(a: &[u8], b: &[u8]) -> Option<(usize, usize)> {
    let at = a.iter().zip(b).position(|(x, y)| x != y)?;
    let count = a.iter().zip(b).filter(|(x, y)| x != y).count();
    Some((at, count))
}

pub fn mismatch_error(at: u64, differing: usize) -> NyError {
    NyError::new(
        ErrorKind::VerifyFailed,
        format!(
            "The drive does not hold what was written. It first differs from the image at {} (byte {}); {} byte{} in that block differ.",
            human_size(at),
            at,
            differing,
            if differing == 1 { "" } else { "s" }
        ),
    )
    .with_hint("Write the image again. If it fails again the drive is probably failing or is not as large as it claims: NyFormat's bad-block check can confirm that.")
}

/// Compare `target` with the image in `source`.
pub fn verify_image(source: ImageSource, target: &Target, opts: &VerifyOptions, cancel: Arc<AtomicBool>, emit: &mut dyn FnMut(Event)) -> Result<VerifyReport> {
    let info = source.info.clone();
    if let (Some(size), Some(cap)) = (info.decompressed_size, target.capacity) {
        if size > cap {
            return Err(NyError::new(ErrorKind::NoSpace, format!("The image ({}) is larger than {} ({}), so it cannot have been written there.", human_size(size), target.display, human_size(cap))));
        }
    }
    emit(Event::Phase { phase: Phase::Verifying, detail: "Reading the drive back".into() });

    let on_device = target.kind == TargetKind::BlockDevice;
    let mut direct = (opts.direct_io || cfg!(target_os = "macos")) && on_device;
    let file = match sys::open_read(&target.path, target.kind, direct) {
        Ok(f) => f,
        Err(e) if direct && e.raw_os_error() == Some(22) => {
            direct = false;
            sys::open_read(&target.path, target.kind, false).map_err(|e| NyError::from_io(&e, &format!("opening {}", target.display)))?
        }
        Err(e) => return Err(NyError::from_io(&e, &format!("opening {}", target.display))),
    };
    if on_device {
        sys::drop_cache(&file);
    }
    let sector = target.drive.as_ref().map_or(512, |d| d.sector_size.max(512)) as u64;
    let chunk_size = round_up(opts.chunk_size.max(ALIGN) as u64, ALIGN as u64) as usize;

    let (free_tx, free_rx) = unbounded::<AlignedBuf>();
    for _ in 0..6 {
        let _ = free_tx.send(AlignedBuf::new(chunk_size));
    }
    let (tx, rx) = bounded::<Msg>(4);
    let stop = AtomicBool::new(false);
    let started = Instant::now();
    let consumed = source.consumed.clone();
    let total_known = info.decompressed_size;
    let compressed_total = info.compressed_size;
    let bytes_done = AtomicU64::new(0);
    let mut meter = SpeedMeter::new(Duration::from_secs(3));
    let mut drive_buf = AlignedBuf::new(chunk_size);
    let mut last_emit = Instant::now() - Duration::from_secs(1);
    let mut volatile = Volatile::default();
    let mut tally = Tally::default();

    let result: Result<u64> = std::thread::scope(|scope| {
        let (stop_ref, cancel_ref) = (&stop, &*cancel);
        let mut reader = source.reader;
        let free_for_reader = free_rx.clone();
        scope.spawn(move || decode_image(&mut reader, chunk_size, free_for_reader, tx, stop_ref, cancel_ref));

        let outcome = (|| -> Result<u64> {
            let mut offset_done = 0u64;
            loop {
                let msg = match rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(m) => m,
                    Err(RecvTimeoutError::Timeout) => {
                        if cancel.load(Ordering::Relaxed) {
                            return Err(NyError::cancelled());
                        }
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => {
                        return Err(if cancel.load(Ordering::Relaxed) { NyError::cancelled() } else { NyError::other("internal error: the image reader stopped") });
                    }
                };
                match msg {
                    Msg::Eof => return Ok(offset_done),
                    Msg::Failed(e) => return Err(e),
                    Msg::Chunk { buf, len, offset } => {
                        if cancel.load(Ordering::Relaxed) {
                            return Err(NyError::cancelled());
                        }
                        if let Some(cap) = target.capacity {
                            if offset + len as u64 > cap {
                                return Err(NyError::new(ErrorKind::NoSpace, format!("The image is larger than {} ({}).", target.display, human_size(cap))));
                            }
                        }
                        let n = read_len(len, offset, direct, sector, target.capacity);
                        if let Err(e) = read_exact_at(&file, &mut drive_buf.as_mut_slice()[..n], offset) {
                            return Err(if e.kind() == std::io::ErrorKind::UnexpectedEof {
                                NyError::new(ErrorKind::VerifyFailed, format!("The drive ends after {}, but the image is longer.", human_size(offset)))
                                    .with_hint("The drive is smaller than it claims, or the write did not finish.")
                            } else {
                                NyError::from_io(&e, &format!("reading {} back", target.display))
                            });
                        }
                        if offset == 0 {
                            volatile = Volatile::from_head(&buf.as_slice()[..len], total_known);
                        }
                        if first_difference(&buf.as_slice()[..len], &drive_buf.as_slice()[..len]).is_some() {
                            if let Err((at, count)) = settle(&volatile, offset, &buf.as_slice()[..len], &drive_buf.as_slice()[..len], &mut tally) {
                                return Err(mismatch_error(offset + at as u64, count));
                            }
                        }
                        let _ = free_tx.send(buf);
                        offset_done = offset + len as u64;
                        bytes_done.store(offset_done, Ordering::Relaxed);
                        if last_emit.elapsed() >= opts.progress_interval {
                            last_emit = Instant::now();
                            let speed = meter.sample(Instant::now(), offset_done);
                            let (fraction, eta) = match (total_known, compressed_total) {
                                (Some(t), _) if t > 0 => ((offset_done as f64 / t as f64).clamp(0.0, 1.0), (speed > 1.0).then(|| t.saturating_sub(offset_done) as f64 / speed)),
                                (_, Some(ct)) if ct > 0 => {
                                    let f = (consumed.load(Ordering::Relaxed) as f64 / ct as f64).clamp(0.0, 1.0);
                                    let el = started.elapsed().as_secs_f64();
                                    (f, (f > 0.01).then(|| el / f - el))
                                }
                                _ => (0.0, None),
                            };
                            emit(Event::Progress(Progress {
                                bytes_done: offset_done,
                                bytes_total: total_known,
                                fraction,
                                speed_bps: speed,
                                eta_secs: eta,
                                targets: vec![TargetProgress { target: target.display.clone(), bytes: offset_done, state: TargetState::Writing }],
                            }));
                        }
                    }
                }
            }
        })();
        stop.store(true, Ordering::SeqCst);
        drop(rx);
        outcome
    });

    let bytes = result?;
    let elapsed = started.elapsed();
    let speed = if elapsed.as_secs_f64() > 0.0 { bytes as f64 / elapsed.as_secs_f64() } else { 0.0 };
    emit(Event::Progress(Progress {
        bytes_done: bytes,
        bytes_total: Some(bytes),
        fraction: 1.0,
        speed_bps: speed,
        eta_secs: Some(0.0),
        targets: vec![TargetProgress { target: target.display.clone(), bytes, state: TargetState::Done }],
    }));
    let note = tally.note();
    emit(Event::Verified { bytes, elapsed_secs: elapsed.as_secs_f64(), avg_speed_bps: speed, note: note.clone() });
    Ok(VerifyReport { bytes, elapsed, note })
}

/// Decode the image into aligned chunks, in order, until it ends.
fn decode_image(src: &mut Box<dyn Read + Send>, chunk_size: usize, free_rx: crossbeam_channel::Receiver<AlignedBuf>, tx: crossbeam_channel::Sender<Msg>, stop: &AtomicBool, cancel: &AtomicBool) {
    let mut offset = 0u64;
    loop {
        if stop.load(Ordering::Relaxed) || cancel.load(Ordering::Relaxed) {
            return;
        }
        let mut buf = match free_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(b) => b,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let mut filled = 0;
        {
            let slice = buf.as_mut_slice();
            while filled < chunk_size {
                match src.read(&mut slice[filled..chunk_size]) {
                    Ok(0) => break,
                    Ok(n) => filled += n,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => {
                        let _ = tx.send(Msg::Failed(map_read_error(&e, offset + filled as u64)));
                        return;
                    }
                }
            }
        }
        if filled == 0 {
            let _ = tx.send(Msg::Eof);
            return;
        }
        let mut pending = Msg::Chunk { buf, len: filled, offset };
        loop {
            match tx.send_timeout(pending, Duration::from_millis(100)) {
                Ok(()) => break,
                Err(crossbeam_channel::SendTimeoutError::Timeout(m)) => {
                    if stop.load(Ordering::Relaxed) || cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    pending = m;
                }
                Err(crossbeam_channel::SendTimeoutError::Disconnected(_)) => return,
            }
        }
        offset += filled as u64;
        if filled < chunk_size {
            let _ = tx.send(Msg::Eof);
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image;
    use crate::writer::{write_image, WriteOptions};
    use std::path::PathBuf;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nyforge-verify-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn pattern(len: usize) -> Vec<u8> {
        let mut x = 0x1234_5678_u64;
        (0..len)
            .map(|_| {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                (x >> 33) as u8
            })
            .collect()
    }

    /// Writes `image` to a file standing in for a drive and returns the drive path.
    fn flash(d: &std::path::Path, image: &std::path::Path) -> PathBuf {
        let drive = d.join("drive.img");
        let t = Target::simulated_drive(&drive, 64 << 20);
        write_image(image::open(image.to_str().unwrap()).unwrap(), vec![t], &WriteOptions { direct_io: false, ..Default::default() }, Arc::new(AtomicBool::new(false)), &mut |_| {}).unwrap();
        drive
    }

    fn verify(image: &std::path::Path, drive: &std::path::Path) -> (Result<VerifyReport>, Vec<Event>) {
        let mut ev = vec![];
        let r = verify_image(
            image::open(image.to_str().unwrap()).unwrap(),
            &Target::simulated_drive(drive, 64 << 20),
            &VerifyOptions { direct_io: false, ..Default::default() },
            Arc::new(AtomicBool::new(false)),
            &mut |e| ev.push(e),
        );
        (r, ev)
    }

    #[test]
    fn a_good_write_verifies_and_reports_the_size() {
        let d = dir("good");
        let img = d.join("a.img");
        std::fs::write(&img, pattern(10 << 20)).unwrap();
        let drive = flash(&d, &img);
        let (r, ev) = verify(&img, &drive);
        assert_eq!(r.unwrap().bytes, 10 << 20);
        assert!(matches!(ev.last().unwrap(), Event::Verified { bytes, .. } if *bytes == 10 << 20));
        assert!(ev.iter().any(|e| matches!(e, Event::Phase { phase: Phase::Verifying, .. })));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn one_flipped_bit_anywhere_is_found_and_located() {
        let d = dir("flip");
        let img = d.join("a.img");
        std::fs::write(&img, pattern(9 << 20)).unwrap();
        let drive = flash(&d, &img);
        for at in [0u64, 511, 4 << 20, (9 << 20) - 1] {
            let mut raw = std::fs::read(&drive).unwrap();
            raw[at as usize] ^= 0x01;
            std::fs::write(&drive, &raw).unwrap();
            let (r, ev) = verify(&img, &drive);
            let e = r.unwrap_err();
            assert_eq!(e.kind, ErrorKind::VerifyFailed, "{}", e.message);
            assert!(e.message.contains(&format!("byte {at}")), "{} (wanted byte {at})", e.message);
            assert!(!ev.iter().any(|e| matches!(e, Event::Verified { .. })));
            raw[at as usize] ^= 0x01;
            std::fs::write(&drive, &raw).unwrap();
        }
        assert!(verify(&img, &drive).0.is_ok());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_drive_that_stops_short_is_reported() {
        let d = dir("short");
        let img = d.join("a.img");
        std::fs::write(&img, pattern(6 << 20)).unwrap();
        let drive = flash(&d, &img);
        let f = std::fs::OpenOptions::new().write(true).open(&drive).unwrap();
        f.set_len(5 << 20).unwrap();
        let e = verify(&img, &drive).0.unwrap_err();
        assert_eq!(e.kind, ErrorKind::VerifyFailed, "{}", e.message);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn compressed_images_are_compared_after_unpacking() {
        let d = dir("xz");
        let raw = pattern(5 << 20);
        let plain = d.join("p.img");
        std::fs::write(&plain, &raw).unwrap();
        let xz = d.join("p.img.xz");
        let mut enc = xz2::write::XzEncoder::new(std::fs::File::create(&xz).unwrap(), 1);
        std::io::Write::write_all(&mut enc, &raw).unwrap();
        enc.finish().unwrap();
        let drive = flash(&d, &xz);
        assert_eq!(std::fs::read(&drive).unwrap(), raw);
        assert!(verify(&xz, &drive).0.is_ok());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn zero_regions_that_became_holes_still_compare_equal() {
        let d = dir("holes");
        let mut raw = pattern(2 << 20);
        raw.extend(vec![0u8; 9 << 20]);
        raw.extend(pattern(1 << 20));
        let img = d.join("h.img");
        std::fs::write(&img, &raw).unwrap();
        let drive = flash(&d, &img);
        assert!(verify(&img, &drive).0.is_ok());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_different_image_does_not_verify() {
        let d = dir("other");
        let a = d.join("a.img");
        let b = d.join("b.img");
        std::fs::write(&a, pattern(3 << 20)).unwrap();
        let mut other = pattern(3 << 20);
        other[1_000_000] ^= 0xFF;
        std::fs::write(&b, other).unwrap();
        let drive = flash(&d, &a);
        let e = verify(&b, &drive).0.unwrap_err();
        assert!(e.message.contains("byte 1000000"), "{}", e.message);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn cancel_stops_the_comparison() {
        let d = dir("cancel");
        let img = d.join("a.img");
        std::fs::write(&img, pattern(8 << 20)).unwrap();
        let drive = flash(&d, &img);
        let r = verify_image(
            image::open(img.to_str().unwrap()).unwrap(),
            &Target::simulated_drive(&drive, 64 << 20),
            &VerifyOptions { direct_io: false, ..Default::default() },
            Arc::new(AtomicBool::new(true)),
            &mut |_| {},
        );
        assert_eq!(r.unwrap_err().kind, ErrorKind::Cancelled);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn raw_handles_are_asked_for_whole_sectors_but_never_past_the_end() {
        assert_eq!(read_len(1000, 0, false, 512, None), 1000);
        assert_eq!(read_len(1000, 0, true, 512, None), 1024);
        assert_eq!(read_len(4096, 0, true, 4096, None), 4096);
        // 1000 bytes at the very end of a 1 MiB drive: ask for the rest of the drive only.
        assert_eq!(read_len(1000, (1 << 20) - 1024, true, 512, Some(1 << 20)), 1024);
        assert_eq!(read_len(1024, (1 << 20) - 1024, true, 512, Some(1 << 20)), 1024);
    }

    /// An image with an MBR whose partition 1 is an EFI partition at sector 8 (4 KiB) of 16 sectors, and partition 2
    /// a BSD slice at sector 64 of 64 sectors.
    fn hybrid_image(len: usize) -> Vec<u8> {
        let mut b = pattern(len);
        b[..512].fill(0);
        let mut put = |slot: usize, ty: u8, start: u32, n: u32| {
            let e = 446 + slot * 16;
            b[e + 4] = ty;
            b[e + 8..e + 12].copy_from_slice(&start.to_le_bytes());
            b[e + 12..e + 16].copy_from_slice(&n.to_le_bytes());
        };
        put(0, 0xEF, 8, 16);
        put(1, 0xA5, 64, 64);
        b[510] = 0x55;
        b[511] = 0xAA;
        b
    }

    #[test]
    fn the_partitions_another_system_rewrites_are_read_from_the_image() {
        let v = Volatile::from_head(&hybrid_image(1 << 20), None);
        assert_eq!(v.covers(8 * 512), Some(VolatileKind::Efi));
        assert_eq!(v.covers(8 * 512 + 3), Some(VolatileKind::Efi));
        assert_eq!(v.covers(24 * 512), None);
        assert_eq!(v.covers(64 * 512), Some(VolatileKind::Bsd));
        assert_eq!(v.covers(441), Some(VolatileKind::DiskId));
        assert_eq!(v.covers(100), None);
        assert!(v.has_partitions() && v.has_bsd());
        // no partition table, nothing is forgiven
        assert_eq!(Volatile::from_head(&pattern(4096)[..], None).covers(8 * 512), None);
    }

    #[test]
    fn a_gpt_efi_partition_is_found_by_its_type_guid() {
        assert_eq!(guid_bytes("C12A7328-F81F-11D2-BA4B-00A0C93EC93B"), [0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B]);
        let mut b = vec![0u8; 64 * 1024];
        b[446 + 4] = 0xEE;
        b[446 + 8] = 1;
        b[446 + 12..446 + 16].copy_from_slice(&100u32.to_le_bytes());
        b[510] = 0x55;
        b[511] = 0xAA;
        b[512..520].copy_from_slice(b"EFI PART");
        b[512 + 72..512 + 80].copy_from_slice(&2u64.to_le_bytes());
        b[512 + 80..512 + 84].copy_from_slice(&4u32.to_le_bytes());
        b[512 + 84..512 + 88].copy_from_slice(&128u32.to_le_bytes());
        let e = 1024;
        b[e..e + 16].copy_from_slice(&guid_bytes("C12A7328-F81F-11D2-BA4B-00A0C93EC93B"));
        b[e + 32..e + 40].copy_from_slice(&40u64.to_le_bytes());
        b[e + 40..e + 48].copy_from_slice(&59u64.to_le_bytes());
        let v = Volatile::from_head(&b, Some(100 * 512));
        assert_eq!(v.covers(40 * 512), Some(VolatileKind::Efi));
        assert_eq!(v.covers(59 * 512 + 511), Some(VolatileKind::Efi));
        assert_eq!(v.covers(60 * 512), None);
        assert_eq!(v.covers(1100), Some(VolatileKind::GptTable));
        assert_eq!(v.covers(99 * 512), Some(VolatileKind::GptTable), "the backup table at the end of the image");
    }

    #[test]
    fn a_few_bytes_changed_inside_an_efi_partition_verify_with_a_note() {
        let d = dir("efi-touch");
        let img = d.join("void.img");
        std::fs::write(&img, hybrid_image(2 << 20)).unwrap();
        let drive = flash(&d, &img);
        let mut raw = std::fs::read(&drive).unwrap();
        for i in 3..11 {
            raw[8 * 512 + i] ^= 0xFF; // the OEM name of the FAT boot sector
        }
        std::fs::write(&drive, &raw).unwrap();
        let (r, ev) = verify(&img, &drive);
        let rep = r.unwrap();
        let note = rep.note.expect("a note");
        assert!(note.contains("8 bytes") && note.contains("EFI") && note.contains(&format!("byte {}", 8 * 512 + 3)), "{note}");
        assert!(ev.iter().any(|e| matches!(e, Event::Verified { note: Some(_), .. })));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_change_outside_those_partitions_still_fails_even_next_to_one() {
        let d = dir("efi-and-more");
        let img = d.join("void.img");
        std::fs::write(&img, hybrid_image(2 << 20)).unwrap();
        let drive = flash(&d, &img);
        let mut raw = std::fs::read(&drive).unwrap();
        raw[8 * 512 + 3] ^= 0xFF; // allowed
        raw[24 * 512] ^= 0x01; // the first byte after the EFI partition: not allowed
        std::fs::write(&drive, &raw).unwrap();
        let (r, _) = verify(&img, &drive);
        let e = r.unwrap_err();
        assert_eq!(e.kind, ErrorKind::VerifyFailed);
        assert!(e.message.contains(&format!("byte {}", 24 * 512)), "{}", e.message);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_partition_that_differs_a_lot_is_a_failure_not_a_rewrite() {
        let d = dir("efi-wrecked");
        let img = d.join("bsd.img");
        let mut bytes = hybrid_image(8 << 20);
        // a big BSD slice: partition 2 now covers 1 MiB
        bytes[446 + 16 + 12..446 + 16 + 16].copy_from_slice(&2048u32.to_le_bytes());
        std::fs::write(&img, bytes).unwrap();
        let drive = flash(&d, &img);
        let mut raw = std::fs::read(&drive).unwrap();
        for b in &mut raw[64 * 512..64 * 512 + 600 * 1024] {
            *b ^= 0xFF;
        }
        std::fs::write(&drive, &raw).unwrap();
        let (r, _) = verify(&img, &drive);
        assert_eq!(r.unwrap_err().kind, ErrorKind::VerifyFailed);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn the_disk_id_that_windows_rewrites_is_allowed() {
        let d = dir("diskid");
        let img = d.join("a.img");
        std::fs::write(&img, hybrid_image(1 << 20)).unwrap();
        let drive = flash(&d, &img);
        let mut raw = std::fs::read(&drive).unwrap();
        raw[440..444].copy_from_slice(&[1, 2, 3, 4]);
        std::fs::write(&drive, &raw).unwrap();
        let (r, _) = verify(&img, &drive);
        assert!(r.unwrap().note.unwrap().contains("disk ID"));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn the_heads_up_is_for_the_bsds_and_void_only() {
        let img = hybrid_image(1 << 20);
        assert!(verify_heads_up(&img, "FreeBSD-14.2-RELEASE-amd64-memstick.img", None).is_some());
        assert!(verify_heads_up(&img, "void-live-x86_64-20250202-base.iso", None).is_some());
        let mut plain = img.clone();
        plain[446 + 16 + 4] = 0; // no BSD slice
        assert!(verify_heads_up(&plain, "archlinux.iso", None).is_none());
        assert!(verify_heads_up(&plain, "void-live.iso", None).is_some());
        assert!(verify_heads_up(&pattern(4096), "void.iso", None).is_none());
    }
}
