//! The capacity check: does the drive hold what it says it holds?

use super::{Subject, TestEvent};
use crate::error::{NyError, Result};
use crate::nyboot::io::FileIo;
use crate::usbformat::capacity::{self, CapacityReport, CapacityTest};
use serde::{Deserialize, Serialize};
use std::sync::atomic::AtomicBool;
use std::time::Instant;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrityResult {
    pub report: CapacityReport,
    pub write_mbps: f64,
    pub read_mbps: f64,
    pub seconds: f64,
}

/// Overwrites the whole drive (or samples spread over it, in the quick mode).
pub fn run(s: &Subject, mode: CapacityTest, unmount: bool, cancel: &AtomicBool, emit: &mut dyn FnMut(TestEvent)) -> Result<IntegrityResult> {
    let (Some(dev), Some(drive)) = (&s.device, &s.drive) else {
        return Err(NyError::invalid("The capacity check needs a whole drive."));
    };
    if mode == CapacityTest::Off {
        return Err(NyError::invalid("No capacity check was chosen."));
    }
    let _prep = crate::sys::prepare_device_for_erase(drive, unmount)?;
    let file = crate::sys::open_rw_direct(dev).map_err(|e| NyError::from_io(&e, &format!("opening {}", dev.display())))?;
    let io = FileIo::new(file, 0, (s.size / 512) * 512, 4096);
    let started = Instant::now();
    let mut stage_started = Instant::now();
    let mut stage = String::new();
    let mut w_mbps = 0.0;
    let mut r_mbps = 0.0;
    let mut last_emit = Instant::now();
    let rep = capacity::run(&io, mode, crate::usbformat::prng::random_seed(), cancel, &mut |st, done, total| {
        if st != stage {
            stage = st.to_string();
            stage_started = Instant::now();
        }
        let el = stage_started.elapsed().as_secs_f64().max(0.001);
        let mbps = done as f64 / el / 1e6;
        if st == "write" {
            w_mbps = mbps;
        } else {
            r_mbps = mbps;
        }
        if last_emit.elapsed().as_millis() >= 200 || done >= total {
            last_emit = Instant::now();
            let half = if st == "write" { 0.0 } else { 0.5 };
            emit(TestEvent::NytestLive {
                step: "integrity".into(),
                label: if st == "write" { "Writing test data".into() } else { "Reading it back".into() },
                fraction: half + 0.5 * (done as f64 / total.max(1) as f64),
                mbps,
                iops: 0.0,
            });
        }
    })?;
    drop(io);
    crate::sys::finish_device(dev);
    Ok(IntegrityResult { report: rep, write_mbps: w_mbps, read_mbps: r_mbps, seconds: started.elapsed().as_secs_f64() })
}
