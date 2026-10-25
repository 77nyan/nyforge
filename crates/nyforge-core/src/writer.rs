//! The write engine.
//!
//! One reader thread decompresses the image into page-aligned chunks taken
//! from a small recycling pool.  Every chunk is shared (`Arc`) with one writer
//! thread per drive, so N drives are flashed in parallel from a single read of
//! the source, and the slowest drive sets the pace.  Zero-filled chunks are
//! turned into sparse seeks (files) or hardware write-zeroes (devices) instead
//! of being sent over a slow USB link.

use crate::alignedbuf::{is_all_zero, AlignedBuf, ALIGN};
use crate::device::{Drive, ResolvedTarget};
use crate::error::{ErrorKind, NyError, Result};
use crate::format::{human_size, round_up};
use crate::image::ImageSource;
use crate::progress::*;
use crate::sys::{self, write_all_at, TargetKind};
use crossbeam_channel::{bounded, unbounded, Receiver, RecvTimeoutError, SendTimeoutError, Sender};
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct Target {
    pub path: PathBuf,
    pub display: String,
    pub kind: TargetKind,
    /// Capacity limit in bytes (the device size). `None` for ordinary files.
    pub capacity: Option<u64>,
    pub drive: Option<Drive>,
}

impl Target {
    pub fn from_resolved(r: ResolvedTarget) -> Self {
        match r {
            ResolvedTarget::Device(d) => Target {
                path: PathBuf::from(&d.path),
                display: d.path.clone(),
                kind: TargetKind::BlockDevice,
                capacity: Some(d.size),
                drive: Some(d),
            },
            ResolvedTarget::File { path } => Target::file(path),
        }
    }

    pub fn file(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        Target { display: path.display().to_string(), path, kind: TargetKind::File, capacity: None, drive: None }
    }

    /// A plain file that behaves like a drive of `capacity` bytes. Used by tests and demos.
    pub fn simulated_drive(path: impl Into<PathBuf>, capacity: u64) -> Self {
        let mut t = Target::file(path);
        t.capacity = Some(capacity);
        t
    }
}

#[derive(Debug, Clone)]
pub struct WriteOptions {
    pub chunk_size: usize,
    pub queue_depth: usize,
    pub direct_io: bool,
    pub skip_zeros: bool,
    pub unmount: bool,
    /// Clear the last MiB of the drive so a stale backup partition table can't confuse the OS.
    pub wipe_tail: bool,
    pub progress_interval: Duration,
}

impl Default for WriteOptions {
    fn default() -> Self {
        Self {
            chunk_size: 4 << 20,
            queue_depth: 4,
            direct_io: true,
            skip_zeros: true,
            unmount: true,
            wipe_tail: true,
            progress_interval: Duration::from_millis(120),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TargetReport {
    pub target: String,
    pub ok: bool,
    pub bytes: u64,
    pub error: Option<NyError>,
}

#[derive(Debug, Clone)]
pub struct WriteReport {
    pub image_bytes: u64,
    pub elapsed: Duration,
    pub targets: Vec<TargetReport>,
}

impl WriteReport {
    pub fn all_ok(&self) -> bool {
        self.targets.iter().all(|t| t.ok)
    }
}

// ------------------------------------------------------------------ internals

struct Chunk {
    buf: Option<AlignedBuf>,
    len: usize,
    padded: usize,
    offset: u64,
    zero: bool,
    pool: Sender<AlignedBuf>,
}

impl Chunk {
    fn data(&self) -> &[u8] {
        &self.buf.as_ref().unwrap().as_slice()[..self.len]
    }
    fn padded_data(&self) -> &[u8] {
        &self.buf.as_ref().unwrap().as_slice()[..self.padded]
    }
}

impl Drop for Chunk {
    fn drop(&mut self) {
        if let Some(b) = self.buf.take() {
            let _ = self.pool.send(b);
        }
    }
}

const ST_WAITING: u8 = 0;
const ST_WRITING: u8 = 1;
const ST_FINISHING: u8 = 2;
const ST_DONE: u8 = 3;
const ST_FAILED: u8 = 4;

struct TargetShared {
    bytes: AtomicU64,
    state: AtomicU8,
}

struct Shared {
    read_bytes: AtomicU64,
    total_len: AtomicU64,
    eof_ok: AtomicBool,
    targets: Vec<TargetShared>,
}

enum ReaderEnd {
    Eof,
    Cancelled,
    Failed(NyError),
    NobodyListening,
}

fn map_read_error(e: &std::io::Error, at: u64) -> NyError {
    use std::io::ErrorKind as K;
    match e.kind() {
        K::InvalidData | K::UnexpectedEof => NyError::new(
            ErrorKind::ImageCorrupt,
            format!("The image is damaged or incomplete (stopped decoding after {}).", human_size(at)),
        )
        .with_hint("Download it again; if it keeps happening the source file itself is bad."),
        K::TimedOut | K::ConnectionReset | K::ConnectionAborted | K::BrokenPipe => NyError::new(
            ErrorKind::Network,
            format!("The download was interrupted after {}: {e}", human_size(at)),
        )
        .with_hint("Check your connection and try again, or download the file first."),
        _ => NyError::from_io(e, "reading the image"),
    }
}

#[allow(clippy::too_many_arguments)]
fn reader_thread(
    mut src: Box<dyn Read + Send>,
    chunk_size: usize,
    free_rx: Receiver<AlignedBuf>,
    pool_tx: Sender<AlignedBuf>,
    txs: Vec<Sender<Arc<Chunk>>>,
    cancel: &AtomicBool,
    shared: &Shared,
    detect_zero: bool,
) -> ReaderEnd {
    let mut alive: Vec<Option<Sender<Arc<Chunk>>>> = txs.into_iter().map(Some).collect();
    let mut offset = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return ReaderEnd::Cancelled;
        }
        let mut buf = match free_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(b) => b,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return ReaderEnd::NobodyListening,
        };
        let mut filled = 0;
        {
            let slice = buf.as_mut_slice();
            while filled < chunk_size {
                match src.read(&mut slice[filled..chunk_size]) {
                    Ok(0) => break,
                    Ok(n) => filled += n,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => return ReaderEnd::Failed(map_read_error(&e, offset + filled as u64)),
                }
            }
        }
        if filled == 0 {
            break;
        }
        let padded = round_up(filled as u64, ALIGN as u64) as usize;
        let zero;
        {
            let slice = buf.as_mut_slice();
            if padded > filled {
                slice[filled..padded].fill(0);
            }
            zero = detect_zero && is_all_zero(&slice[..filled]);
        }
        shared.read_bytes.fetch_add(filled as u64, Ordering::Relaxed);
        let chunk = Arc::new(Chunk { buf: Some(buf), len: filled, padded, offset, zero, pool: pool_tx.clone() });

        let mut any = false;
        for slot in alive.iter_mut() {
            let Some(tx) = slot else { continue };
            let mut pending = chunk.clone();
            loop {
                match tx.send_timeout(pending, Duration::from_millis(100)) {
                    Ok(()) => {
                        any = true;
                        break;
                    }
                    Err(SendTimeoutError::Timeout(c)) => {
                        if cancel.load(Ordering::Relaxed) {
                            return ReaderEnd::Cancelled;
                        }
                        pending = c;
                    }
                    Err(SendTimeoutError::Disconnected(_)) => {
                        *slot = None;
                        break;
                    }
                }
            }
        }
        if !any {
            return ReaderEnd::NobodyListening;
        }
        offset += filled as u64;
        if filled < chunk_size {
            break;
        }
    }
    shared.total_len.store(offset, Ordering::SeqCst);
    shared.eof_ok.store(true, Ordering::SeqCst);
    ReaderEnd::Eof
}

struct Outcome {
    bytes: u64,
    error: Option<NyError>,
}

fn target_thread(
    idx: usize,
    t: &Target,
    rx: Receiver<Arc<Chunk>>,
    shared: &Shared,
    cancel: &AtomicBool,
    opts: &WriteOptions,
) -> Outcome {
    let me = &shared.targets[idx];
    let fail = |e: NyError, bytes: u64| {
        me.state.store(ST_FAILED, Ordering::SeqCst);
        Outcome { bytes, error: Some(e) }
    };
    let what = format!("writing to {}", t.display);
    me.state.store(ST_WRITING, Ordering::SeqCst);

    // macOS's raw disk node only takes whole sectors, so there it is not optional.
    let want_direct = (opts.direct_io || cfg!(target_os = "macos")) && t.kind == TargetKind::BlockDevice;
    let mut direct = want_direct;
    let file = match sys::open_target(&t.path, t.kind, direct) {
        Ok(f) => f,
        Err(e) if direct && e.raw_os_error() == Some(22) => {
            // EINVAL: the device does not do direct I/O. Fall back to buffered writes.
            direct = false;
            match sys::open_target(&t.path, t.kind, false) {
                Ok(f) => f,
                Err(e) => return fail(NyError::from_io(&e, &format!("opening {}", t.display)), 0),
            }
        }
        Err(e) => return fail(NyError::from_io(&e, &format!("opening {}", t.display)), 0),
    };

    let mut written_end = 0u64;
    let mut since_sync = 0u64;
    let mut zero_unsupported = false;
    loop {
        let chunk = match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(c) => c,
            Err(RecvTimeoutError::Timeout) => {
                if cancel.load(Ordering::Relaxed) {
                    return fail(NyError::cancelled(), written_end);
                }
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if cancel.load(Ordering::Relaxed) {
            return fail(NyError::cancelled(), written_end);
        }
        let end = chunk.offset + chunk.len as u64;
        if let Some(cap) = t.capacity {
            if end > cap {
                return fail(
                    NyError::new(
                        ErrorKind::NoSpace,
                        format!(
                            "The image is larger than {} ({} of space).",
                            t.display,
                            human_size(cap)
                        ),
                    )
                    .with_hint("Use a bigger drive."),
                    written_end,
                );
            }
        }

        let mut handled = false;
        if chunk.zero && opts.skip_zeros {
            match t.kind {
                TargetKind::File => handled = true, // becomes a hole; set_len() at the end extends it
                TargetKind::BlockDevice if !zero_unsupported => {
                    let zlen = if direct && t.capacity.is_none_or(|c| chunk.offset + chunk.padded as u64 <= c) {
                        chunk.padded
                    } else {
                        chunk.len
                    } as u64;
                    if sys::zero_range(&file, chunk.offset, zlen) {
                        handled = true;
                    } else {
                        zero_unsupported = true;
                    }
                }
                _ => {}
            }
        }
        if !handled {
            let mut n = if t.kind == TargetKind::File { chunk.len } else if direct { chunk.padded } else { chunk.len };
            if direct {
                if let Some(cap) = t.capacity {
                    if chunk.offset + n as u64 > cap {
                        // The padding would run past the end of the device. Pad only to a whole sector
                        // instead: that is all direct I/O (and macOS's raw disk node) asks for.
                        let sector = t.drive.as_ref().map_or(512, |d| d.sector_size.max(512)) as usize;
                        let to_sector = chunk.len.div_ceil(sector) * sector;
                        if to_sector <= chunk.padded && chunk.offset + to_sector as u64 <= cap {
                            n = to_sector;
                        } else {
                            // Not even that fits: finish with buffered I/O.
                            sys::set_direct(&file, false);
                            direct = false;
                            n = chunk.len;
                        }
                    }
                }
            }
            let data = if n > chunk.len { &chunk.padded_data()[..n] } else { &chunk.data()[..n] };
            if let Err(e) = write_all_at(&file, data, chunk.offset) {
                return fail(NyError::from_io(&e, &what), written_end);
            }
            since_sync += n as u64;
        }
        written_end = end;
        me.bytes.store(written_end, Ordering::Relaxed);

        if !direct && t.kind == TargetKind::BlockDevice && since_sync >= (128 << 20) {
            if let Err(e) = sys::sync_device(&file) {
                return fail(NyError::from_io(&e, &what), written_end);
            }
            since_sync = 0;
        }
    }

    if !shared.eof_ok.load(Ordering::SeqCst) {
        // The reader stopped early (error or cancel); its error is reported separately.
        return fail(NyError::new(ErrorKind::Other, "The write was interrupted before the image finished."), written_end);
    }

    me.state.store(ST_FINISHING, Ordering::SeqCst);
    let total = shared.total_len.load(Ordering::SeqCst);
    if t.kind == TargetKind::File {
        if let Err(e) = file.set_len(total) {
            return fail(NyError::from_io(&e, &what), written_end);
        }
    }
    if let Err(e) = sys::sync_device(&file) {
        return fail(NyError::from_io(&e, "flushing the drive"), written_end);
    }
    drop(file);

    if t.kind == TargetKind::BlockDevice {
        if opts.wipe_tail {
            if let Some(cap) = t.capacity {
                let tail = 1u64 << 20;
                let start = cap.saturating_sub(tail);
                if start >= round_up(total, ALIGN as u64) {
                    if let Ok(f) = sys::open_rw(&t.path, false) {
                        let zeros = vec![0u8; (cap - start) as usize];
                        let _ = write_all_at(&f, &zeros, start);
                        let _ = sys::sync_device(&f);
                    }
                }
            }
        }
        sys::finish_device(&t.path);
    }
    me.bytes.store(total, Ordering::Relaxed);
    me.state.store(ST_DONE, Ordering::SeqCst);
    Outcome { bytes: total, error: None }
}

fn state_of(v: u8) -> TargetState {
    match v {
        ST_WAITING => TargetState::Waiting,
        ST_WRITING => TargetState::Writing,
        ST_FINISHING => TargetState::Finishing,
        ST_DONE => TargetState::Done,
        _ => TargetState::Failed,
    }
}

/// Flash `source` onto every target in parallel.
pub fn write_image(
    source: ImageSource,
    targets: Vec<Target>,
    opts: &WriteOptions,
    cancel: Arc<AtomicBool>,
    emit: &mut dyn FnMut(Event),
) -> Result<WriteReport> {
    if targets.is_empty() {
        return Err(NyError::invalid("No drive selected."));
    }
    for (i, a) in targets.iter().enumerate() {
        if targets[i + 1..].iter().any(|b| b.path == a.path) {
            return Err(NyError::invalid(format!("{} was selected twice.", a.display)));
        }
    }
    let info = source.info.clone();
    if let Some(size) = info.decompressed_size {
        for t in &targets {
            if let Some(cap) = t.capacity {
                if size > cap {
                    return Err(NyError::new(
                        ErrorKind::NoSpace,
                        format!(
                            "The image ({}) does not fit on {} ({}).",
                            human_size(size),
                            t.display,
                            human_size(cap)
                        ),
                    )
                    .with_hint("Pick a bigger drive."));
                }
            }
        }
    }

    emit(Event::Start {
        image: info.label.clone(),
        container: info.container.name().to_string(),
        total_bytes: info.decompressed_size,
        targets: targets.iter().map(|t| t.display.clone()).collect(),
    });
    let touches_drives = targets.iter().any(|t| t.drive.is_some());
    emit(Event::Phase {
        phase: Phase::Preparing,
        detail: if touches_drives { "Releasing the drives".into() } else { "Preparing".into() },
    });

    // Unmount / lock volumes. Guards stay alive until the end of the function.
    let mut _preps = Vec::new();
    for t in &targets {
        if let Some(d) = &t.drive {
            let prep = sys::prepare_device(d, opts.unmount)?;
            if !prep.unmounted.is_empty() {
                emit(Event::Log { message: format!("Unmounted {} from {}", prep.unmounted.join(", "), d.path) });
            }
            _preps.push(prep);
        }
    }

    let n = targets.len();
    let shared = Shared {
        read_bytes: AtomicU64::new(0),
        total_len: AtomicU64::new(0),
        eof_ok: AtomicBool::new(false),
        targets: (0..n).map(|_| TargetShared { bytes: AtomicU64::new(0), state: AtomicU8::new(ST_WAITING) }).collect(),
    };

    let chunk_size = round_up(opts.chunk_size.max(ALIGN) as u64, ALIGN as u64) as usize;
    let pool_size = opts.queue_depth + n + 2;
    let (pool_tx, free_rx) = unbounded::<AlignedBuf>();
    for _ in 0..pool_size {
        let _ = pool_tx.send(AlignedBuf::new(chunk_size));
    }
    let mut txs = Vec::new();
    let mut rxs = Vec::new();
    for _ in 0..n {
        let (tx, rx) = bounded::<Arc<Chunk>>(opts.queue_depth);
        txs.push(tx);
        rxs.push(rx);
    }

    let started = Instant::now();
    let mut meter = SpeedMeter::new(Duration::from_secs(3));
    let total_known = info.decompressed_size;
    let consumed = source.consumed.clone();
    let compressed_total = info.compressed_size;
    emit(Event::Phase { phase: Phase::Writing, detail: "Writing".into() });

    let mut reader_end: Option<ReaderEnd> = None;
    let mut outcomes: Vec<Option<Outcome>> = (0..n).map(|_| None).collect();
    let mut reported = vec![false; n];

    std::thread::scope(|scope| {
        let shared_ref = &shared;
        let cancel_ref: &AtomicBool = &cancel;
        let detect_zero = opts.skip_zeros;
        let reader = source.reader;
        let pool_for_reader = pool_tx.clone();
        let rh = scope.spawn(move || {
            reader_thread(reader, chunk_size, free_rx, pool_for_reader, txs, cancel_ref, shared_ref, detect_zero)
        });
        drop(pool_tx);

        let mut handles = Vec::new();
        for (i, (t, rx)) in targets.iter().zip(rxs).enumerate() {
            handles.push(scope.spawn(move || target_thread(i, t, rx, shared_ref, cancel_ref, opts)));
        }

        let mut handles: Vec<Option<std::thread::ScopedJoinHandle<'_, Outcome>>> = handles.into_iter().map(Some).collect();
        let mut reader_handle = Some(rh);
        loop {
            std::thread::sleep(opts.progress_interval);
            // collect finished targets
            for i in 0..n {
                if handles[i].as_ref().is_some_and(|h| h.is_finished()) {
                    let o = handles[i].take().unwrap().join().unwrap_or(Outcome {
                        bytes: 0,
                        error: Some(NyError::other("internal error: writer thread panicked")),
                    });
                    emit(Event::TargetFinished {
                        target: targets[i].display.clone(),
                        ok: o.error.is_none(),
                        bytes: o.bytes,
                        error: o.error.clone(),
                    });
                    reported[i] = true;
                    outcomes[i] = Some(o);
                }
            }
            if reader_handle.as_ref().is_some_and(|h| h.is_finished()) {
                reader_end = Some(reader_handle.take().unwrap().join().unwrap_or(ReaderEnd::Failed(NyError::other("internal error: reader thread panicked"))));
            }

            // snapshot
            let tp: Vec<TargetProgress> = (0..n)
                .map(|i| TargetProgress {
                    target: targets[i].display.clone(),
                    bytes: shared.targets[i].bytes.load(Ordering::Relaxed),
                    state: state_of(shared.targets[i].state.load(Ordering::SeqCst)),
                })
                .collect();
            let live: Vec<u64> = tp.iter().filter(|t| t.state != TargetState::Failed).map(|t| t.bytes).collect();
            let slowest = live.iter().copied().min().unwrap_or(0);
            let mut speed = meter.sample(Instant::now(), slowest);
            if speed <= 0.0 && slowest > 0 {
                // Too early for the moving window: use the average so far.
                speed = slowest as f64 / started.elapsed().as_secs_f64().max(0.001);
            }
            let (fraction, eta) = match (total_known, compressed_total) {
                (Some(total), _) if total > 0 => {
                    let f = (slowest as f64 / total as f64).clamp(0.0, 1.0);
                    let eta = if speed > 1.0 { Some((total.saturating_sub(slowest)) as f64 / speed) } else { None };
                    (f, eta)
                }
                (_, Some(ct)) if ct > 0 => {
                    let f = (consumed.load(Ordering::Relaxed) as f64 / ct as f64).clamp(0.0, 1.0);
                    let el = started.elapsed().as_secs_f64();
                    let eta = if f > 0.01 { Some(el / f - el) } else { None };
                    (f, eta)
                }
                _ => (0.0, None),
            };
            emit(Event::Progress(Progress {
                bytes_done: slowest,
                bytes_total: total_known,
                fraction,
                speed_bps: speed,
                eta_secs: eta,
                targets: tp,
            }));

            if handles.iter().all(|h| h.is_none()) {
                break;
            }
        }
        if let Some(h) = reader_handle.take() {
            reader_end = Some(h.join().unwrap_or(ReaderEnd::Failed(NyError::other("internal error: reader thread panicked"))));
        }
    });

    let elapsed = started.elapsed();
    let image_bytes = shared.total_len.load(Ordering::SeqCst);

    match reader_end {
        Some(ReaderEnd::Failed(e)) => {
            emit(Event::Error { error: e.clone() });
            return Err(e);
        }
        Some(ReaderEnd::Cancelled) => {
            let e = NyError::cancelled();
            emit(Event::Error { error: e.clone() });
            return Err(e);
        }
        _ => {}
    }
    if cancel.load(Ordering::Relaxed) {
        let e = NyError::cancelled();
        emit(Event::Error { error: e.clone() });
        return Err(e);
    }

    let reports: Vec<TargetReport> = targets
        .iter()
        .zip(outcomes)
        .map(|(t, o)| {
            let o = o.unwrap_or(Outcome { bytes: 0, error: Some(NyError::other("target never finished")) });
            TargetReport { target: t.display.clone(), ok: o.error.is_none(), bytes: o.bytes, error: o.error }
        })
        .collect();
    let ok = reports.iter().all(|r| r.ok);
    emit(Event::Finished {
        ok,
        bytes: image_bytes,
        elapsed_secs: elapsed.as_secs_f64(),
        avg_speed_bps: if elapsed.as_secs_f64() > 0.0 { image_bytes as f64 / elapsed.as_secs_f64() } else { 0.0 },
    });
    Ok(WriteReport { image_bytes, elapsed, targets: reports })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nyforge-wr-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| ((i.wrapping_mul(2654435761) >> 7) & 0xff) as u8).collect()
    }

    fn run(src: ImageSource, targets: Vec<Target>) -> (Result<WriteReport>, Vec<Event>) {
        let mut ev = Vec::new();
        let r = write_image(src, targets, &WriteOptions::default(), Arc::new(AtomicBool::new(false)), &mut |e| ev.push(e));
        (r, ev)
    }

    #[test]
    fn writes_identical_copies_to_several_targets() {
        let d = tdir("multi");
        let data = pattern(9 * 1024 * 1024 + 123); // not a multiple of anything
        let img = d.join("in.img");
        std::fs::write(&img, &data).unwrap();
        let outs: Vec<PathBuf> = (0..3).map(|i| d.join(format!("out{i}.img"))).collect();
        let (r, ev) = run(crate::image::open_file(&img).unwrap(), outs.iter().map(Target::file).collect());
        let r = r.unwrap();
        assert!(r.all_ok());
        assert_eq!(r.image_bytes, data.len() as u64);
        for o in &outs {
            assert_eq!(std::fs::read(o).unwrap(), data);
        }
        assert!(ev.iter().any(|e| matches!(e, Event::Progress(_))));
        assert!(matches!(ev.last().unwrap(), Event::Finished { ok: true, .. }));
    }

    #[test]
    fn decompresses_xz_on_the_fly() {
        let d = tdir("xz");
        let data = pattern(3_000_000);
        let mut enc = xz2::write::XzEncoder::new(Vec::new(), 1);
        enc.write_all(&data).unwrap();
        let img = d.join("in.img.xz");
        std::fs::write(&img, enc.finish().unwrap()).unwrap();
        let out = d.join("out.img");
        let (r, _) = run(crate::image::open_file(&img).unwrap(), vec![Target::file(&out)]);
        assert!(r.unwrap().all_ok());
        assert_eq!(std::fs::read(&out).unwrap(), data);
    }

    // Sparse-file accounting (`st_blocks`) exists on Unix only.
    #[cfg(unix)]
    #[test]
    fn zero_regions_become_holes() {
        use std::os::unix::fs::MetadataExt;
        let d = tdir("sparse");
        let mut data = vec![0u8; 40 * 1024 * 1024];
        data[..4096].copy_from_slice(&pattern(4096));
        let n = data.len();
        data[n - 4096..].copy_from_slice(&pattern(4096));
        let img = d.join("in.img");
        std::fs::write(&img, &data).unwrap();
        let out = d.join("out.img");
        let (r, _) = run(crate::image::open_file(&img).unwrap(), vec![Target::file(&out)]);
        assert!(r.unwrap().all_ok());
        let md = std::fs::metadata(&out).unwrap();
        assert_eq!(md.len(), data.len() as u64, "logical size preserved");
        assert!(md.blocks() * 512 < 10 * 1024 * 1024, "only {} of {} bytes should be allocated", md.blocks() * 512, md.len());
        assert_eq!(std::fs::read(&out).unwrap(), data);
    }

    #[test]
    fn too_small_drive_is_refused_before_writing() {
        let d = tdir("small");
        let data = pattern(2 * 1024 * 1024);
        let img = d.join("in.img");
        std::fs::write(&img, &data).unwrap();
        let out = d.join("tiny.img");
        let (r, ev) = run(
            crate::image::open_file(&img).unwrap(),
            vec![Target::simulated_drive(&out, 1024 * 1024)],
        );
        let e = r.err().unwrap();
        assert_eq!(e.kind, ErrorKind::NoSpace);
        assert!(e.message.contains("does not fit"), "{}", e.message);
        assert!(!out.exists(), "must not touch the target");
        assert!(ev.is_empty());
    }

    #[test]
    fn unknown_size_overflow_is_caught_while_writing() {
        let d = tdir("overflow");
        let data = pattern(5 * 1024 * 1024);
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(&data).unwrap();
        let img = d.join("in.img.gz"); // gzip: size unknown up front
        std::fs::write(&img, enc.finish().unwrap()).unwrap();
        let out = d.join("tiny.img");
        let (r, _) = run(
            crate::image::open_file(&img).unwrap(),
            vec![Target::simulated_drive(&out, 1024 * 1024)],
        );
        let r = r.unwrap();
        assert!(!r.all_ok());
        assert_eq!(r.targets[0].error.as_ref().unwrap().kind, ErrorKind::NoSpace);
    }

    #[test]
    fn one_bad_target_does_not_stop_the_others() {
        let d = tdir("partial");
        let data = pattern(6 * 1024 * 1024);
        let img = d.join("in.img");
        std::fs::write(&img, &data).unwrap();
        let good = d.join("good.img");
        let bad = d.join("no-such-dir").join("bad.img");
        let (r, _) = run(crate::image::open_file(&img).unwrap(), vec![Target::file(&bad), Target::file(&good)]);
        let r = r.unwrap();
        assert!(!r.targets[0].ok);
        assert!(r.targets[1].ok);
        assert_eq!(std::fs::read(&good).unwrap(), data);
    }

    #[test]
    fn corrupt_archive_is_reported_as_corrupt() {
        let d = tdir("corrupt");
        let data = pattern(4_000_000);
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(&data).unwrap();
        let mut bytes = enc.finish().unwrap();
        bytes.truncate(bytes.len() / 2);
        let img = d.join("cut.img.gz");
        std::fs::write(&img, bytes).unwrap();
        let out = d.join("out.img");
        let (r, _) = run(crate::image::open_file(&img).unwrap(), vec![Target::file(&out)]);
        assert_eq!(r.err().unwrap().kind, ErrorKind::ImageCorrupt);
    }

    #[test]
    fn cancel_stops_promptly() {
        let d = tdir("cancel");
        let data = pattern(64 * 1024 * 1024);
        let img = d.join("in.img");
        std::fs::write(&img, &data).unwrap();
        let out = d.join("out.img");
        let cancel = Arc::new(AtomicBool::new(false));
        let c2 = cancel.clone();
        let mut opts = WriteOptions::default();
        opts.chunk_size = 1 << 20;
        let mut seen = 0;
        let r = write_image(
            crate::image::open_file(&img).unwrap(),
            vec![Target::file(&out)],
            &opts,
            cancel,
            &mut |e| {
                if let Event::Progress(p) = e {
                    seen += 1;
                    if p.bytes_done > 0 || seen > 1 {
                        c2.store(true, Ordering::Relaxed);
                    }
                }
            },
        );
        assert_eq!(r.err().unwrap().kind, ErrorKind::Cancelled);
    }

    #[test]
    fn same_target_twice_is_rejected() {
        let d = tdir("dup");
        let img = d.join("in.img");
        std::fs::write(&img, b"x").unwrap();
        let out = d.join("o.img");
        let (r, _) = run(crate::image::open_file(&img).unwrap(), vec![Target::file(&out), Target::file(&out)]);
        assert_eq!(r.err().unwrap().kind, ErrorKind::InvalidInput);
    }
}
