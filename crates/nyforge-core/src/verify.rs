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
                        if let Some((at, count)) = first_difference(&buf.as_slice()[..len], &drive_buf.as_slice()[..len]) {
                            return Err(mismatch_error(offset + at as u64, count));
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
    emit(Event::Verified { bytes, elapsed_secs: elapsed.as_secs_f64(), avg_speed_bps: speed });
    Ok(VerifyReport { bytes, elapsed })
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
}
