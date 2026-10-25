//! Counterfeit-capacity detection.
//!
//! Fake "1 TB" sticks are small flash chips with firmware that lies about the size. They fail in one
//! of two ways, and both make you lose data silently weeks later:
//!
//! * **wrap-around**: writing past the real end overwrites the beginning;
//! * **black hole**: writes past the real end are dropped and reads return junk.
//!
//! The *full* test writes unique pseudo-random data into every block of the drive and reads it all back
//! (it needs about twice the time a plain write of the whole drive takes) — it is exact.
//! The *quick* test does the same for 4 KiB samples spread over the drive (about 128 MiB of traffic at
//! most). It catches the usual fakes (whose real size is a multiple of the sample spacing), but it is a
//! sampling test: a pass is "found nothing", not proof.

use super::prng::block_pattern;
use crate::error::{ErrorKind, NyError, Result};
use crate::nyboot::io::Io;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapacityTest {
    Off,
    Quick,
    Full,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapacityReport {
    pub mode: CapacityTest,
    pub claimed: u64,
    pub tested_bytes: u64,
    pub bad_blocks: u64,
    pub first_bad: Option<u64>,
    pub last_bad: Option<u64>,
    pub genuine: bool,
    /// Best guess of what the drive really holds (equals `claimed` when genuine).
    pub estimated_real: u64,
}

const MIB: u64 = 1 << 20;
const QUICK_BLOCK: u64 = 4096;
const QUICK_MAX_SAMPLES: u64 = 32768;
/// Below this size the quick test has too little to sample: use the full one.
pub const QUICK_MIN_BYTES: u64 = 8 * MIB;

fn check(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(NyError::cancelled())
    } else {
        Ok(())
    }
}

fn io_fail(e: std::io::Error, what: &str) -> NyError {
    NyError::from_io(&e, what)
}

/// Which (offset, length) blocks a run covers.
fn plan(len: u64, mode: CapacityTest) -> (Vec<(u64, u64)>, CapacityTest, u64) {
    let mode = if mode == CapacityTest::Quick && len < QUICK_MIN_BYTES { CapacityTest::Full } else { mode };
    match mode {
        CapacityTest::Full => {
            let n = len.div_ceil(MIB);
            let blocks = (0..n).map(|i| (i * MIB, (len - i * MIB).min(MIB))).collect();
            (blocks, mode, MIB)
        }
        _ => {
            let n = (len / MIB).min(QUICK_MAX_SAMPLES);
            let step = ((len / n) & !(MIB - 1)).max(MIB);
            let mut blocks: Vec<(u64, u64)> = (0..n).map(|i| (i * step, QUICK_BLOCK)).filter(|(o, _)| o + QUICK_BLOCK <= len).collect();
            let tail = (len - QUICK_BLOCK) & !(QUICK_BLOCK - 1);
            if blocks.last().map(|b| b.0) != Some(tail) {
                blocks.push((tail, QUICK_BLOCK));
            }
            (blocks, CapacityTest::Quick, step)
        }
    }
}

/// Turn the list of bad block offsets into a verdict. `step` is the distance between tested blocks.
pub fn assess(mode: CapacityTest, claimed: u64, tested_bytes: u64, bad: &[u64], step: u64) -> CapacityReport {
    let mut bad = bad.to_vec();
    bad.sort_unstable();
    let (first, last) = (bad.first().copied(), bad.last().copied());
    let estimated_real = match (first, last) {
        (None, _) | (_, None) => claimed,
        // The start of the drive got overwritten: the data past the real end wrapped around onto it.
        // The wrapped part is [0, claimed - real), so the real size is what is left after its end.
        (Some(0), Some(l)) => claimed.saturating_sub((l + step).min(claimed)),
        // Everything from some point on is gone: the drive ends there.
        (Some(f), _) => f,
    };
    CapacityReport {
        mode,
        claimed,
        tested_bytes,
        bad_blocks: bad.len() as u64,
        first_bad: first,
        last_bad: last,
        genuine: bad.is_empty(),
        estimated_real,
    }
}

/// Run the test over all of `io` (which must not be cached: open the drive with direct I/O).
/// `progress(stage, done_bytes, total_bytes)` is called with stage "write" and then "read".
/// This destroys everything on the drive.
pub fn run(
    io: &dyn Io,
    mode: CapacityTest,
    seed: u64,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&str, u64, u64),
) -> Result<CapacityReport> {
    assert!(mode != CapacityTest::Off);
    let len = io.len();
    let (blocks, mode, step) = plan(len, mode);
    let total: u64 = blocks.iter().map(|b| b.1).sum();
    let mut buf = vec![0u8; MIB as usize];
    let mut done = 0u64;
    for (i, &(off, n)) in blocks.iter().enumerate() {
        check(cancel)?;
        block_pattern(seed, off / QUICK_BLOCK, &mut buf[..n as usize]);
        io.write_at(&buf[..n as usize], off).map_err(|e| io_fail(e, "writing the capacity test"))?;
        done += n;
        if i % 16 == 0 || n == MIB {
            progress("write", done, total);
        }
    }
    io.sync().map_err(|e| io_fail(e, "flushing the capacity test"))?;
    progress("write", total, total);

    let mut expect = vec![0u8; MIB as usize];
    let mut bad = vec![];
    done = 0;
    for (i, &(off, n)) in blocks.iter().enumerate() {
        check(cancel)?;
        block_pattern(seed, off / QUICK_BLOCK, &mut expect[..n as usize]);
        match io.read_at(&mut buf[..n as usize], off) {
            Ok(()) if buf[..n as usize] == expect[..n as usize] => {}
            Ok(()) => bad.push(off),
            // A block that cannot be read at all is as lost as one that reads back wrong.
            Err(_) => bad.push(off),
        }
        done += n;
        if i % 16 == 0 || n == MIB {
            progress("read", done, total);
        }
    }
    progress("read", total, total);
    Ok(assess(mode, len, total, &bad, step))
}

impl CapacityReport {
    /// The error to stop with when the drive is not what it claims to be.
    pub fn to_error(&self) -> NyError {
        use crate::format::human_size;
        let how = match self.mode {
            CapacityTest::Full => "Every block was written and read back.",
            _ => "Spot check of the whole drive.",
        };
        NyError::new(
            ErrorKind::FakeCapacity,
            format!(
                "This drive says it holds {} but it does not: {} of {} test blocks came back wrong. It probably holds about {}.",
                human_size(self.claimed),
                self.bad_blocks,
                self.tested_bytes.div_ceil(if self.mode == CapacityTest::Full { MIB } else { QUICK_BLOCK }),
                human_size(self.estimated_real),
            ),
        )
        .with_hint(format!(
            "{how} Do not trust it with data you care about. {}",
            if self.mode == CapacityTest::Full {
                "To use only the part that works, run the format again with --fit-real-capacity."
            } else {
                "A full test (--test-capacity full) measures the real size exactly and enables --fit-real-capacity."
            }
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Clone, Copy)]
    enum Fake {
        Honest,
        Wrap,
        BlackHole,
    }

    /// A drive that claims `claimed` bytes but holds `real`.
    struct FakeDrive {
        mem: RefCell<Vec<u8>>,
        claimed: u64,
        real: u64,
        kind: Fake,
    }

    impl FakeDrive {
        fn new(claimed: u64, real: u64, kind: Fake) -> Self {
            FakeDrive { mem: RefCell::new(vec![0; real as usize]), claimed, real, kind }
        }
    }

    impl Io for FakeDrive {
        fn len(&self) -> u64 {
            self.claimed
        }
        fn read_at(&self, buf: &mut [u8], off: u64) -> std::io::Result<()> {
            let mem = self.mem.borrow();
            for (i, b) in buf.iter_mut().enumerate() {
                let p = off + i as u64;
                *b = match self.kind {
                    Fake::Honest => mem[p as usize],
                    Fake::Wrap => mem[(p % self.real) as usize],
                    Fake::BlackHole => if p < self.real { mem[p as usize] } else { 0xEE },
                };
            }
            Ok(())
        }
        fn write_at(&self, buf: &[u8], off: u64) -> std::io::Result<()> {
            let mut mem = self.mem.borrow_mut();
            for (i, b) in buf.iter().enumerate() {
                let p = off + i as u64;
                match self.kind {
                    Fake::Honest => mem[p as usize] = *b,
                    Fake::Wrap => mem[(p % self.real) as usize] = *b,
                    Fake::BlackHole => if p < self.real { mem[p as usize] = *b },
                }
            }
            Ok(())
        }
        fn sync(&self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn go(d: &FakeDrive, mode: CapacityTest) -> CapacityReport {
        run(d, mode, 99, &AtomicBool::new(false), &mut |_, _, _| {}).unwrap()
    }

    #[test]
    fn an_honest_drive_passes_both_tests() {
        for mode in [CapacityTest::Quick, CapacityTest::Full] {
            let r = go(&FakeDrive::new(64 * MIB, 64 * MIB, Fake::Honest), mode);
            assert!(r.genuine, "{mode:?}");
            assert_eq!(r.estimated_real, 64 * MIB);
        }
    }

    #[test]
    fn full_test_measures_a_wrap_around_fake_exactly() {
        let r = go(&FakeDrive::new(64 * MIB, 24 * MIB, Fake::Wrap), CapacityTest::Full);
        assert!(!r.genuine);
        assert_eq!(r.estimated_real, 24 * MIB);
        assert_eq!(r.first_bad, Some(0));
        let e = r.to_error();
        assert_eq!(e.kind, ErrorKind::FakeCapacity);
        assert!(e.message.contains("40 of 64 test blocks") && e.message.contains("25.2 MB"), "{}", e.message);
    }

    #[test]
    fn full_test_measures_a_black_hole_fake_exactly() {
        let r = go(&FakeDrive::new(64 * MIB, 40 * MIB, Fake::BlackHole), CapacityTest::Full);
        assert!(!r.genuine);
        assert_eq!(r.estimated_real, 40 * MIB);
    }

    #[test]
    fn quick_test_catches_both_kinds() {
        let w = go(&FakeDrive::new(128 * MIB, 24 * MIB, Fake::Wrap), CapacityTest::Quick);
        assert!(!w.genuine);
        assert_eq!(w.mode, CapacityTest::Quick);
        // sampling: the estimate is within one sample step (1 MiB here)
        assert!(w.estimated_real.abs_diff(24 * MIB) <= MIB, "{}", w.estimated_real);
        let b = go(&FakeDrive::new(128 * MIB, 40 * MIB, Fake::BlackHole), CapacityTest::Quick);
        assert!(!b.genuine);
        assert!(b.estimated_real.abs_diff(40 * MIB) <= MIB);
    }

    #[test]
    fn quick_on_a_tiny_drive_becomes_full() {
        let r = go(&FakeDrive::new(4 * MIB, 4 * MIB, Fake::Honest), CapacityTest::Quick);
        assert_eq!(r.mode, CapacityTest::Full);
        assert!(r.genuine);
    }

    #[test]
    fn quick_test_touches_a_small_part_of_the_drive() {
        let r = go(&FakeDrive::new(256 * MIB, 256 * MIB, Fake::Honest), CapacityTest::Quick);
        assert!(r.tested_bytes <= 129 * QUICK_BLOCK * 256 / 128 + QUICK_BLOCK * 300, "{}", r.tested_bytes);
        assert!(r.tested_bytes < 4 * MIB);
    }

    #[test]
    fn cancelling_stops_the_test() {
        let d = FakeDrive::new(64 * MIB, 64 * MIB, Fake::Honest);
        let cancel = AtomicBool::new(false);
        let mut n = 0;
        let e = run(&d, CapacityTest::Full, 1, &cancel, &mut |_, _, _| {
            n += 1;
            if n == 3 {
                cancel.store(true, Ordering::Relaxed);
            }
        })
        .err()
        .unwrap();
        assert_eq!(e.kind, ErrorKind::Cancelled);
    }

    #[test]
    fn assess_estimates_from_bad_positions() {
        let r = assess(CapacityTest::Full, 100 * MIB, 100 * MIB, &[0, MIB, 2 * MIB], MIB);
        assert_eq!(r.estimated_real, 97 * MIB);
        let r = assess(CapacityTest::Full, 100 * MIB, 100 * MIB, &[60 * MIB, 61 * MIB], MIB);
        assert_eq!(r.estimated_real, 60 * MIB);
        assert!(assess(CapacityTest::Full, 100 * MIB, 100 * MIB, &[], MIB).genuine);
    }
}
