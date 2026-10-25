//! Progress reporting.  Events are plain data: the CLI prints them, `--json`
//! serialises them one per line, and the GUI reads exactly the same stream.

use crate::error::NyError;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetState {
    Waiting,
    Writing,
    Finishing,
    Done,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetProgress {
    pub target: String,
    pub bytes: u64,
    pub state: TargetState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Progress {
    /// Image bytes written by the slowest drive that is still going.
    pub bytes_done: u64,
    pub bytes_total: Option<u64>,
    /// 0.0 ..= 1.0
    pub fraction: f64,
    pub speed_bps: f64,
    pub eta_secs: Option<f64>,
    pub targets: Vec<TargetProgress>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Preparing,
    Writing,
    Finalizing,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Start {
        image: String,
        container: String,
        total_bytes: Option<u64>,
        targets: Vec<String>,
    },
    Phase {
        phase: Phase,
        detail: String,
    },
    Progress(Progress),
    TargetFinished {
        target: String,
        ok: bool,
        bytes: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<NyError>,
    },
    Finished {
        ok: bool,
        bytes: u64,
        elapsed_secs: f64,
        avg_speed_bps: f64,
    },
    Error {
        error: NyError,
    },
    Log {
        message: String,
    },
}

/// Moving-window throughput estimate (smooth, but reacts within a second or two).
pub struct SpeedMeter {
    window: Duration,
    samples: VecDeque<(Instant, u64)>,
}

impl SpeedMeter {
    pub fn new(window: Duration) -> Self {
        Self { window, samples: VecDeque::new() }
    }

    pub fn sample(&mut self, now: Instant, total_bytes: u64) -> f64 {
        self.samples.push_back((now, total_bytes));
        while let Some(&(t, _)) = self.samples.front() {
            if now.duration_since(t) > self.window && self.samples.len() > 2 {
                self.samples.pop_front();
            } else {
                break;
            }
        }
        let (t0, b0) = *self.samples.front().unwrap();
        let dt = now.duration_since(t0).as_secs_f64();
        if dt < 0.05 {
            return 0.0;
        }
        (total_bytes.saturating_sub(b0)) as f64 / dt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meter_measures_rate() {
        let mut m = SpeedMeter::new(Duration::from_secs(3));
        let t0 = Instant::now();
        let mut speed = 0.0;
        for i in 0..=20u64 {
            speed = m.sample(t0 + Duration::from_millis(i * 100), i * 10_000_000);
        }
        assert!((speed - 100_000_000.0).abs() < 1_000_000.0, "{speed}");
    }

    #[test]
    fn event_json_shape() {
        let e = Event::Finished { ok: true, bytes: 5, elapsed_secs: 1.0, avg_speed_bps: 5.0 };
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains("\"event\":\"finished\""));
    }
}
