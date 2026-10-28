//! The sustained-write test: write for a long time and draw the speed.
//!
//! Flash drives write fast into a small cache of fast memory first and slow down, sometimes by 10×, when it is full or the
//! controller gets hot. A 1 GB benchmark finishes inside the cache and never shows it. The graph does.

use super::engine::{run_job, Access, Job, Limits, Op, Pattern};
use super::speed::{open_bench, Bench};
use super::{check, Place, Subject, TestEvent};
use crate::error::Result;
use crate::format::human_size;
use serde::{Deserialize, Serialize};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct SustainedOptions {
    /// How much to write
    pub size: u64,
    /// Stop after this long even if `size` is not reached
    pub max_seconds: f64,
    pub block: usize,
    pub qd: usize,
}

impl Default for SustainedOptions {
    fn default() -> Self {
        SustainedOptions { size: 4 << 30, max_seconds: 300.0, block: 1 << 20, qd: 4 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cliff {
    /// How much had been written when the speed fell
    pub at_bytes: u64,
    pub at_secs: f64,
    pub before_mbps: f64,
    pub after_mbps: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SustainedResult {
    pub bytes: u64,
    pub seconds: f64,
    pub avg_mbps: f64,
    /// the first seconds
    pub start_mbps: f64,
    /// the last quarter
    pub end_mbps: f64,
    pub min_mbps: f64,
    pub cliff: Option<Cliff>,
    /// (seconds, MB/s), at most 600 points
    pub samples: Vec<(f64, f64)>,
}

/// Smooth, then find where the speed falls and stays down.
pub fn analyze(samples: &[(f64, f64)], total_bytes: u64, seconds: f64) -> SustainedResult {
    let n = samples.len();
    let speeds: Vec<f64> = samples.iter().map(|s| s.1).collect();
    let mean = |v: &[f64]| if v.is_empty() { 0.0 } else { v.iter().sum::<f64>() / v.len() as f64 };
    // a moving average over ~2 s (8 samples at 250 ms)
    let w = 8usize;
    let smooth: Vec<f64> = (0..n)
        .map(|i| {
            let lo = i.saturating_sub(w / 2);
            let hi = (i + w / 2 + 1).min(n);
            mean(&speeds[lo..hi])
        })
        .collect();
    let head = (n / 10).clamp(4.min(n), 40.min(n.max(1)));
    let start = mean(&speeds[..head.min(n)]);
    let tail = (n / 4).max(1);
    let end = mean(&speeds[n.saturating_sub(tail)..]);
    let min = smooth.iter().copied().skip(head.min(n)).fold(f64::MAX, f64::min);
    let min = if min == f64::MAX { smooth.iter().copied().fold(f64::MAX, f64::min) } else { min };
    // The speed "before": the median of the early part. A cliff is a stretch of >= 3 s below 60% of that.
    let mut cliff = None;
    if n >= 40 {
        let mut early: Vec<f64> = smooth[w..(n / 4).max(w + 4).min(n)].to_vec();
        early.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let base = early[early.len() / 2];
        if base >= 1.0 {
            let run = 12usize; // 3 s
            let mut i = w;
            while i + run <= n {
                if smooth[i..i + run].iter().all(|&v| v < base * 0.6) {
                    // the speed at which it really left: the first sample that dropped below 80%
                    let mut at = i;
                    while at > w && smooth[at - 1] < base * 0.8 {
                        at -= 1;
                    }
                    let after = mean(&speeds[(i + run / 2).min(n)..]);
                    if after < base * 0.6 {
                        let before = mean(&speeds[w.min(at)..at.max(w.min(at) + 1).min(n)]);
                        let t = samples[at.min(n - 1)].0;
                        let bytes = ((total_bytes as f64) * (t / seconds.max(0.001))).min(total_bytes as f64) as u64;
                        // cumulative bytes are better than a proportion when the speed changed: integrate
                        let mut acc = 0.0;
                        let mut prev_t = 0.0;
                        for s in &samples[..at.min(n)] {
                            acc += s.1 * 1e6 * (s.0 - prev_t);
                            prev_t = s.0;
                        }
                        let bytes = if acc > 0.0 { acc as u64 } else { bytes };
                        cliff = Some(Cliff { at_bytes: bytes, at_secs: t, before_mbps: before, after_mbps: after });
                    }
                    break;
                }
                i += 1;
            }
        }
    }
    // thin the graph
    let step = n.div_ceil(600).max(1);
    let thin: Vec<(f64, f64)> = samples.chunks(step).map(|c| (c[c.len() - 1].0, mean(&c.iter().map(|x| x.1).collect::<Vec<_>>()))).collect();
    let avg = if seconds > 0.0 { total_bytes as f64 / seconds / 1e6 } else { 0.0 };
    // Too short to have been sampled (a fast drive and a small test): the average is all there is.
    let (start, end, min) = if n == 0 { (avg, avg, avg) } else { (start, end, if min < f64::MAX { min } else { avg }) };
    SustainedResult {
        bytes: total_bytes,
        seconds,
        avg_mbps: avg,
        start_mbps: start,
        end_mbps: end,
        min_mbps: min,
        cliff,
        samples: thin,
    }
}

pub fn run(s: &Subject, place: &Place, o: &SustainedOptions, direct: bool, unmount: bool, cancel: &AtomicBool, emit: &mut dyn FnMut(TestEvent)) -> Result<(SustainedResult, Vec<String>)> {
    let mut notes = vec![];
    // No filling: the writes create the data. A file simply grows; a device is overwritten from the start.
    let mut bench: Bench = match place {
        Place::File(_) => open_empty_file(s, place, o.size, direct, cancel, emit, &mut notes)?,
        _ => open_bench(s, place, o.size, direct, unmount, false, "sustained", cancel, emit, &mut notes)?,
    };
    check(cancel)?;
    let size = bench.len;
    let job = Job { op: Op::Write, access: Access::Sequential, block: o.block, qd: o.qd, start: bench.start, len: size, pattern: Pattern::Random, wrap: false, sync_each_write: false, seed: crate::usbformat::prng::random_seed() };
    let mut samples: Vec<(f64, f64)> = vec![];
    let (mut last_t, mut last_b) = (0.0f64, 0u64);
    let m = run_job(&bench.file, &job, &Limits { duration: Duration::from_secs_f64(o.max_seconds.max(5.0)), max_bytes: Some(size), cancel }, Duration::from_millis(250), &mut |el, b, ops| {
        let t = el.as_secs_f64();
        let dt = (t - last_t).max(0.001);
        let mbps = (b.saturating_sub(last_b)) as f64 / dt / 1e6;
        samples.push((t, mbps));
        emit(TestEvent::NytestSample { t, mbps, bytes: b });
        emit(TestEvent::NytestLive { step: "sustained".into(), label: format!("Written {}", human_size(b)), fraction: (b as f64 / size as f64).min(1.0), mbps, iops: ops as f64 / t.max(0.001) });
        last_t = t;
        last_b = b;
    })?;
    let seconds = m.elapsed.as_secs_f64();
    bench.writable = true;
    drop(bench);
    if m.bytes < size && seconds >= o.max_seconds {
        notes.push(format!("The sustained write was stopped after {:.0} s with {} written (the time limit).", seconds, human_size(m.bytes)));
    }
    Ok((analyze(&samples, m.bytes, seconds), notes))
}

/// A new empty file for the sustained test (no filling: the test writes it).
fn open_empty_file(s: &Subject, place: &Place, size: u64, direct: bool, _cancel: &AtomicBool, _emit: &mut dyn FnMut(TestEvent), notes: &mut Vec<String>) -> Result<Bench> {
    let _ = s;
    const MIB: u64 = 1 << 20;
    let Place::File(folder) = place else { unreachable!() };
    let mut size = size / MIB * MIB;
    if !folder.is_dir() {
        return Err(crate::error::NyError::invalid(format!("{} is not a folder.", folder.display())));
    }
    if let Some(free) = crate::nytest::util::free_space(folder) {
        let room = free.saturating_sub(64 * MIB) / MIB * MIB;
        if size > room {
            if room < 64 * MIB {
                return Err(crate::error::NyError::new(crate::error::ErrorKind::NoSpace, format!("{} has only {} free: not enough for the sustained write test.", folder.display(), human_size(free))));
            }
            notes.push(format!("The sustained write was limited to {} (all the drive has free, with a little to spare).", human_size(room)));
            size = room;
        }
    }
    let path = folder.join(format!("nytest-sustained-{}-{:08x}.tmp", std::process::id(), (crate::usbformat::prng::random_seed() & 0xFFFF_FFFF) as u32));
    let opened = super::engine::open_test_file(&path, true, true, direct).map_err(|e| crate::error::NyError::from_io(&e, &format!("creating the test file in {}", folder.display())))?;
    Ok(Bench::temp_file(opened.file, opened.direct, size, path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn series(f: impl Fn(f64) -> f64, secs: f64) -> Vec<(f64, f64)> {
        let n = (secs / 0.25) as usize;
        (1..=n).map(|i| (i as f64 * 0.25, f(i as f64 * 0.25))).collect()
    }

    #[test]
    fn finds_where_the_fast_cache_ends() {
        // 90 MB/s for 20 s, then 12 MB/s
        let s = series(|t| if t < 20.0 { 90.0 } else { 12.0 }, 60.0);
        let total: f64 = s.iter().map(|x| x.1 * 1e6 * 0.25).sum();
        let r = analyze(&s, total as u64, 60.0);
        let c = r.cliff.clone().expect("cliff");
        assert!((c.at_secs - 20.0).abs() < 2.5, "{c:?}");
        assert!((c.before_mbps - 90.0).abs() < 5.0 && (c.after_mbps - 12.0).abs() < 3.0, "{c:?}");
        let expect = 90e6 * 20.0;
        assert!((c.at_bytes as f64 - expect).abs() < expect * 0.15, "{} vs {}", c.at_bytes, expect);
        assert!(r.start_mbps > 80.0 && r.end_mbps < 15.0, "{r:?}");
    }

    #[test]
    fn a_steady_drive_has_no_cliff_and_a_noisy_one_does_not_fool_it() {
        let steady = series(|_| 30.0, 40.0);
        assert!(analyze(&steady, 1, 40.0).cliff.is_none());
        // jitter of +-40%, no trend
        let noisy = series(|t| 30.0 + 12.0 * ((t * 7.3).sin()), 80.0);
        assert!(analyze(&noisy, 1, 80.0).cliff.is_none());
        // a short stall (2 s) is not a cliff
        let stall = series(|t| if (20.0..22.0).contains(&t) { 2.0 } else { 40.0 }, 60.0);
        assert!(analyze(&stall, 1, 60.0).cliff.is_none());
        // too short to say
        assert!(analyze(&series(|_| 5.0, 4.0), 1, 4.0).cliff.is_none());
        assert_eq!(analyze(&[], 0, 0.0).samples.len(), 0);
    }

    #[test]
    fn a_run_too_short_to_sample_reports_its_average() {
        let r = analyze(&[], 134_000_000, 0.1);
        assert!((r.avg_mbps - 1340.0).abs() < 1.0);
        assert_eq!((r.start_mbps, r.end_mbps, r.min_mbps), (r.avg_mbps, r.avg_mbps, r.avg_mbps));
    }

    #[test]
    fn graph_is_thinned() {
        let s = series(|_| 10.0, 400.0);
        let r = analyze(&s, 1, 400.0);
        assert!(r.samples.len() <= 600 && r.samples.len() > 300, "{}", r.samples.len());
    }
}
