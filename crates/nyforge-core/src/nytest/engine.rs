//! The I/O engine behind the speed tests.
//!
//! A job is "do reads (or writes) of this size, in this order, with this many in flight, for this long". It is run the way
//! storage benchmarks run: uncached (`O_DIRECT` / `FILE_FLAG_NO_BUFFERING`), with data that cannot be compressed, and with
//! the final flush counted for writes, so a drive cannot look faster than it is by hiding the work in a cache.
//!
//! "Queue depth" is the number of requests in flight. Every request runs on its own thread, each waiting for its own
//! answer, which keeps that many requests outstanding at the drive on every operating system.

use crate::alignedbuf::AlignedBuf;
use crate::error::{NyError, Result};
use crate::usbformat::prng::Rng;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Read,
    Write,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    Sequential,
    Random,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pattern {
    /// Data that cannot be compressed (what real files look like to a drive)
    Random,
    /// All zeros (drives that compress, and the "best case")
    Zero,
}

// ---------------------------------------------------------------------------------------- latency

/// Latencies in microseconds, kept as a log-scale histogram (about 12% resolution) so a test of millions of requests
/// needs no memory.
#[derive(Clone)]
pub struct Hist {
    buckets: Box<[u32; 512]>,
    count: u64,
    sum_us: u128,
    max_us: u64,
}

impl Default for Hist {
    fn default() -> Self {
        Hist { buckets: Box::new([0; 512]), count: 0, sum_us: 0, max_us: 0 }
    }
}

fn bucket_of(us: u64) -> usize {
    if us < 8 {
        return us as usize;
    }
    let l = 63 - us.leading_zeros() as u64;
    let sub = (us >> (l - 3)) & 7;
    (8 + (l - 3) * 8 + sub) as usize
}

fn bucket_low(i: usize) -> u64 {
    if i < 8 {
        return i as u64;
    }
    let l = ((i - 8) / 8 + 3) as u32;
    let sub = ((i - 8) % 8) as u64;
    (8 + sub) << (l - 3)
}

impl Hist {
    pub fn add(&mut self, d: Duration) {
        let us = d.as_micros().min(u64::MAX as u128) as u64;
        let i = bucket_of(us).min(511);
        self.buckets[i] = self.buckets[i].saturating_add(1);
        self.count += 1;
        self.sum_us += us as u128;
        self.max_us = self.max_us.max(us);
    }

    pub fn merge(&mut self, o: &Hist) {
        for (a, b) in self.buckets.iter_mut().zip(o.buckets.iter()) {
            *a = a.saturating_add(*b);
        }
        self.count += o.count;
        self.sum_us += o.sum_us;
        self.max_us = self.max_us.max(o.max_us);
    }

    pub fn count(&self) -> u64 {
        self.count
    }

    /// The latency (µs) that `q` (0–1) of the requests were at or under.
    pub fn quantile(&self, q: f64) -> u64 {
        if self.count == 0 {
            return 0;
        }
        let want = ((self.count as f64 * q).ceil() as u64).clamp(1, self.count);
        let mut seen = 0u64;
        for (i, &n) in self.buckets.iter().enumerate() {
            seen += n as u64;
            if seen >= want {
                // the middle of the bucket, but never above the largest value seen
                let lo = bucket_low(i);
                let hi = if i + 1 < 512 { bucket_low(i + 1) } else { lo };
                return ((lo + hi) / 2).min(self.max_us).max(lo.min(self.max_us));
            }
        }
        self.max_us
    }

    pub fn summary(&self) -> Latency {
        Latency {
            avg_us: if self.count > 0 { (self.sum_us / self.count as u128) as u64 } else { 0 },
            p50_us: self.quantile(0.50),
            p90_us: self.quantile(0.90),
            p99_us: self.quantile(0.99),
            p999_us: self.quantile(0.999),
            max_us: self.max_us,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Latency {
    pub avg_us: u64,
    pub p50_us: u64,
    pub p90_us: u64,
    pub p99_us: u64,
    pub p999_us: u64,
    pub max_us: u64,
}

// ---------------------------------------------------------------------------------------- opening

/// What a file was opened as.
pub struct Opened {
    pub file: File,
    /// Uncached I/O is on. When a file system refuses it, the file is used cached and the test says so.
    pub direct: bool,
}

#[cfg(unix)]
fn direct_flags(direct: bool) -> i32 {
    #[cfg(target_os = "linux")]
    if direct {
        return libc::O_DIRECT;
    }
    let _ = direct;
    0
}

/// Open (or create) a test file on a file system. Falls back to cached I/O when the file system does not do uncached.
pub fn open_test_file(path: &std::path::Path, create: bool, write: bool, want_direct: bool) -> std::io::Result<Opened> {
    let mut o = std::fs::OpenOptions::new();
    o.read(true);
    if write || create {
        o.write(true);
    }
    if create {
        o.create(true).truncate(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        if want_direct {
            let mut od = o.clone();
            od.custom_flags(direct_flags(true));
            match od.open(path) {
                Ok(f) => {
                    #[cfg(target_os = "macos")]
                    crate::sys::set_direct(&f, true);
                    return Ok(Opened { file: f, direct: cfg!(any(target_os = "linux", target_os = "macos")) });
                }
                Err(e) if e.raw_os_error() == Some(22) => {} // EINVAL: this file system has no uncached I/O
                Err(e) => return Err(e),
            }
        }
        let f = o.open(path)?;
        Ok(Opened { file: f, direct: false })
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_NO_BUFFERING: u32 = 0x2000_0000;
        const FILE_FLAG_WRITE_THROUGH: u32 = 0x8000_0000;
        if want_direct {
            let mut od = o.clone();
            od.custom_flags(FILE_FLAG_NO_BUFFERING | FILE_FLAG_WRITE_THROUGH);
            if let Ok(f) = od.open(path) {
                return Ok(Opened { file: f, direct: true });
            }
        }
        Ok(Opened { file: o.open(path)?, direct: false })
    }
}

// ---------------------------------------------------------------------------------------- the job

#[derive(Debug, Clone)]
pub struct Job {
    pub op: Op,
    pub access: Access,
    pub block: usize,
    /// requests in flight
    pub qd: usize,
    /// byte range of the file or device the job works in
    pub start: u64,
    pub len: u64,
    pub pattern: Pattern,
    /// wrap around at the end of the range (false: stop there)
    pub wrap: bool,
    /// ask the drive to put every write on the medium before answering (slow, strict)
    pub sync_each_write: bool,
    pub seed: u64,
}

#[derive(Clone, Default)]
pub struct Measure {
    pub bytes: u64,
    pub ops: u64,
    pub elapsed: Duration,
    pub hist: Hist,
}

impl Measure {
    pub fn mbps(&self) -> f64 {
        let s = self.elapsed.as_secs_f64();
        if s > 0.0 {
            self.bytes as f64 / s / 1e6
        } else {
            0.0
        }
    }
    pub fn iops(&self) -> f64 {
        let s = self.elapsed.as_secs_f64();
        if s > 0.0 {
            self.ops as f64 / s
        } else {
            0.0
        }
    }
}

fn io_error(e: &std::io::Error, op: Op, off: u64) -> NyError {
    let what = match op {
        Op::Read => format!("reading at {off}"),
        Op::Write => format!("writing at {off}"),
    };
    NyError::from_io(e, &what)
}

/// When a job stops.
pub struct Limits<'a> {
    pub duration: Duration,
    /// stop after this many bytes
    pub max_bytes: Option<u64>,
    pub cancel: &'a AtomicBool,
}

/// Run a job. `tick(elapsed, bytes_so_far, ops_so_far)` is called about every `tick_every` while it runs.
pub fn run_job(file: &File, job: &Job, limits: &Limits, tick_every: Duration, tick: &mut dyn FnMut(Duration, u64, u64)) -> Result<Measure> {
    let block = job.block.max(512);
    let nblocks = (job.len / block as u64).max(1);
    let stop = AtomicBool::new(false);
    let next = AtomicU64::new(0);
    let bytes = AtomicU64::new(0);
    let ops = AtomicU64::new(0);
    let failure: Mutex<Option<NyError>> = Mutex::new(None);
    let hists: Mutex<Vec<Hist>> = Mutex::new(vec![]);
    let started = Instant::now();
    let max_bytes = limits.max_bytes.unwrap_or(u64::MAX);

    std::thread::scope(|scope| {
        for t in 0..job.qd.max(1) {
            let (stop, next, bytes, ops, failure, hists) = (&stop, &next, &bytes, &ops, &failure, &hists);
            scope.spawn(move || {
                let mut buf = AlignedBuf::new(block.div_ceil(4096) * 4096);
                let mut rng = Rng::new(job.seed ^ (t as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xA5A5);
                let mut hist = Hist::default();
                while !stop.load(Ordering::Relaxed) {
                    let idx = match job.access {
                        Access::Sequential => {
                            let i = next.fetch_add(1, Ordering::Relaxed);
                            if !job.wrap && i >= nblocks {
                                break;
                            }
                            i % nblocks
                        }
                        Access::Random => rng.next_u64() % nblocks,
                    };
                    let off = job.start + idx * block as u64;
                    let n = block;
                    let t0 = Instant::now();
                    let r = match job.op {
                        Op::Read => crate::sys::read_exact_at(file, &mut buf.as_mut_slice()[..n], off),
                        Op::Write => {
                            if job.pattern == Pattern::Random {
                                // every request carries fresh bytes: nothing for a compressing or deduplicating controller to save
                                rng.fill(&mut buf.as_mut_slice()[..n]);
                            }
                            let w = crate::sys::write_all_at(file, &buf.as_slice()[..n], off);
                            if w.is_ok() && job.sync_each_write {
                                let _ = file.sync_data();
                            }
                            w
                        }
                    };
                    let dt = t0.elapsed();
                    if let Err(e) = r {
                        let mut f = failure.lock().unwrap();
                        if f.is_none() {
                            *f = Some(io_error(&e, job.op, off));
                        }
                        stop.store(true, Ordering::Relaxed);
                        break;
                    }
                    hist.add(dt);
                    bytes.fetch_add(n as u64, Ordering::Relaxed);
                    ops.fetch_add(1, Ordering::Relaxed);
                }
                hists.lock().unwrap().push(hist);
            });
        }
        // the monitor
        let mut last_tick = Instant::now();
        loop {
            std::thread::sleep(Duration::from_millis(10));
            let el = started.elapsed();
            let b = bytes.load(Ordering::Relaxed);
            if limits.cancel.load(Ordering::Relaxed) || el >= limits.duration || b >= max_bytes || stop.load(Ordering::Relaxed) {
                stop.store(true, Ordering::Relaxed);
                break;
            }
            // a sequential job without wrap that has covered its range is finished
            if job.access == Access::Sequential && !job.wrap && next.load(Ordering::Relaxed) >= nblocks + job.qd as u64 {
                stop.store(true, Ordering::Relaxed);
                break;
            }
            if last_tick.elapsed() >= tick_every {
                last_tick = Instant::now();
                tick(el, b, ops.load(Ordering::Relaxed));
            }
        }
    });

    if let Some(e) = failure.into_inner().unwrap() {
        return Err(e);
    }
    if limits.cancel.load(Ordering::Relaxed) {
        return Err(NyError::cancelled());
    }
    // A write is not finished until the drive has it: the flush counts.
    if job.op == Op::Write {
        file.sync_data().map_err(|e| NyError::from_io(&e, "flushing the test writes"))?;
    }
    let elapsed = started.elapsed();
    let mut hist = Hist::default();
    for h in hists.into_inner().unwrap() {
        hist.merge(&h);
    }
    Ok(Measure { bytes: bytes.load(Ordering::Relaxed), ops: ops.load(Ordering::Relaxed), elapsed, hist })
}

/// Fill `[0, len)` of a test file with data (so that reading it back reads real data, not holes).
pub fn fill_file(file: &File, len: u64, cancel: &AtomicBool, progress: &mut dyn FnMut(u64)) -> Result<Duration> {
    let started = Instant::now();
    let chunk = 1usize << 20;
    let mut buf = AlignedBuf::new(chunk);
    let mut rng = Rng::new(crate::usbformat::prng::random_seed());
    let mut off = 0u64;
    let mut last = Instant::now();
    while off < len {
        if cancel.load(Ordering::Relaxed) {
            return Err(NyError::cancelled());
        }
        rng.fill(buf.as_mut_slice());
        let n = (chunk as u64).min(len - off) as usize;
        // keep the request aligned: the file is sized in whole blocks
        let n = n.div_ceil(4096) * 4096;
        crate::sys::write_all_at(file, &buf.as_slice()[..n], off).map_err(|e| io_error(&e, Op::Write, off))?;
        off += n as u64;
        if last.elapsed() >= Duration::from_millis(200) {
            last = Instant::now();
            progress(off.min(len));
        }
    }
    file.sync_data().map_err(|e| NyError::from_io(&e, "flushing the test file"))?;
    progress(len);
    Ok(started.elapsed())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_are_ordered_and_close() {
        let mut last = 0;
        for us in [0u64, 1, 5, 7, 8, 9, 15, 16, 100, 1000, 12345, 1_000_000, 90_000_000] {
            let i = bucket_of(us);
            assert!(i >= last, "{us}");
            last = i;
            let lo = bucket_low(i);
            assert!(lo <= us, "{us}: low {lo}");
            assert!(us - lo <= us / 7 + 1, "{us}: low {lo} is too far");
        }
        assert!(bucket_of(u64::MAX) < 512);
    }

    #[test]
    fn percentiles() {
        let mut h = Hist::default();
        for i in 1..=1000u64 {
            h.add(Duration::from_micros(i * 10));
        }
        let s = h.summary();
        assert!((4500..=5500).contains(&s.p50_us), "{s:?}");
        assert!((8400..=9600).contains(&s.p90_us), "{s:?}");
        assert!((9400..=10000).contains(&s.p99_us), "{s:?}");
        assert_eq!(s.max_us, 10_000);
        assert!(s.avg_us > 4900 && s.avg_us < 5100);
        let mut g = Hist::default();
        g.add(Duration::from_micros(77));
        h.merge(&g);
        assert_eq!(h.count(), 1001);
    }
}
