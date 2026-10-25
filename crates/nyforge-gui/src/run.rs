//! Follows one engine run: folds its events into what the screens need, and decides, honestly, how it ended.

use crate::worker::{Job, Msg};
use nyforge_core::error::{ErrorKind, NyError};
use nyforge_core::progress::{Event, Phase, Progress};

#[derive(Clone, Debug)]
pub struct Failure {
    pub title: String,
    pub message: String,
    pub hint: Option<String>,
    #[allow(dead_code)] // kept so callers can branch on the engine's error kind
    pub kind: Option<ErrorKind>,
}

#[derive(Clone, Debug)]
pub enum Outcome {
    Running,
    Success,
    Cancelled,
    Failed(Failure),
}

pub struct Run {
    pub job: Job,
    pub phase: Option<(Phase, String)>,
    pub progress: Option<Progress>,
    pub total_hint: Option<u64>,
    pub finished: Option<(f64, f64)>, // (elapsed, average speed)
    /// Speed and time left as shown to the person: averaged over several seconds and refreshed once a second.
    pub smooth: Smoother,
    pub error: Option<NyError>,
    pub target_error: Option<NyError>,
    pub stderr: Vec<String>,
    pub json: Vec<serde_json::Value>,
    pub exit: Option<Option<i32>>,
    pub spawn_failed: Option<String>,
    /// For runs that report success only by ending cleanly (listing, removing).
    needs_finished: bool,
    pub started: std::time::Instant,
}

impl Run {
    pub fn new(job: Job, needs_finished: bool) -> Self {
        Run {
            job,
            phase: None,
            progress: None,
            total_hint: None,
            finished: None,
            smooth: Smoother::default(),
            error: None,
            target_error: None,
            stderr: vec![],
            json: vec![],
            exit: None,
            spawn_failed: None,
            needs_finished,
            started: std::time::Instant::now(),
        }
    }

    /// Reads everything the engine has said so far. Returns true if anything arrived.
    pub fn pump(&mut self) -> bool {
        let mut any = false;
        while let Ok(m) = self.job.rx.try_recv() {
            any = true;
            match m {
                Msg::Event(ev) => match ev {
                    Event::Start { total_bytes, .. } => self.total_hint = total_bytes,
                    Event::Phase { phase, detail } => self.phase = Some((phase, detail)),
                    Event::Progress(p) => {
                        self.smooth.feed(p.bytes_done, p.bytes_total.or(self.total_hint));
                        self.progress = Some(p);
                    }
                    Event::TargetFinished { ok, error, .. } => {
                        if !ok {
                            self.target_error = error;
                        }
                    }
                    Event::Finished { ok, elapsed_secs, avg_speed_bps, .. } => {
                        if ok {
                            self.finished = Some((elapsed_secs, avg_speed_bps));
                        }
                    }
                    Event::Error { error } => self.error = Some(error),
                    Event::Log { .. } => {}
                },
                Msg::Json(v) => self.json.push(v),
                Msg::Stderr(l) => {
                    self.stderr.push(l);
                    if self.stderr.len() > 40 {
                        self.stderr.remove(0);
                    }
                }
                Msg::Exit(c) => self.exit = Some(c),
                Msg::SpawnFailed(w) => self.spawn_failed = Some(w),
            }
        }
        any
    }

    pub fn is_done(&self) -> bool {
        self.exit.is_some() || self.spawn_failed.is_some()
    }

    /// The first JSON object line carrying this key (the file listing, the format report).
    pub fn find_json(&self, key: &str) -> Option<&serde_json::Value> {
        self.json.iter().find(|v| v.get(key).is_some())
    }

    pub fn outcome(&self) -> Outcome {
        if let Some(w) = &self.spawn_failed {
            return Outcome::Failed(Failure { title: "Could not start".into(), message: w.clone(), hint: None, kind: None });
        }
        let Some(code) = self.exit else { return Outcome::Running };
        let cancelled = matches!(&self.error, Some(e) if e.kind == ErrorKind::Cancelled) || code == Some(130);
        if cancelled {
            return Outcome::Cancelled;
        }
        let ok_exit = code == Some(0);
        if ok_exit && self.error.is_none() && self.target_error.is_none() && (!self.needs_finished || self.finished.is_some()) {
            return Outcome::Success;
        }
        if let Some(e) = self.error.as_ref().or(self.target_error.as_ref()) {
            return Outcome::Failed(Failure { title: title_for(e.kind), message: e.message.clone(), hint: e.hint.clone(), kind: Some(e.kind) });
        }
        // pkexec's own exit codes: 126 = the prompt was dismissed, 127 = not authorised
        let (title, message, hint) = match code {
            Some(126) => ("Permission prompt dismissed", "The administrator prompt was closed or not completed, so nothing was changed.".to_string(), None),
            Some(127) => ("Not authorised", "The system did not authorise NyForge to access drives. Nothing was changed.".to_string(), Some("Try again and enter an administrator password, or start NyForge with sudo.".to_string())),
            _ => {
                let tail = self.stderr.iter().rev().take(3).rev().cloned().collect::<Vec<_>>().join(" ");
                let msg = if tail.is_empty() {
                    match code {
                        Some(c) => format!("The NyForge engine stopped unexpectedly (exit code {c})."),
                        None => "The NyForge engine was stopped unexpectedly.".to_string(),
                    }
                } else {
                    tail
                };
                ("Something went wrong", msg, None)
            }
        };
        Outcome::Failed(Failure { title: title.into(), message, hint, kind: None })
    }

    /// 0.0 ..= 1.0 when the engine has reported real progress.
    pub fn fraction(&self) -> Option<f32> {
        self.progress.as_ref().map(|p| p.fraction.clamp(0.0, 1.0) as f32)
    }
}

pub fn title_for(k: ErrorKind) -> String {
    match k {
        ErrorKind::Permission => "Not allowed",
        ErrorKind::NoSpace => "Not enough space",
        ErrorKind::Busy => "Drive is busy",
        ErrorKind::DeviceGone => "Drive disappeared",
        ErrorKind::Hardware => "Drive reported a hardware error",
        ErrorKind::ReadOnly => "Drive is write-protected",
        ErrorKind::ImageCorrupt => "Image is damaged",
        ErrorKind::ImageUnsupported => "Image not supported",
        ErrorKind::Network => "Network problem",
        ErrorKind::Cancelled => "Cancelled",
        ErrorKind::Unsafe => "Refused for safety",
        ErrorKind::InvalidInput => "Cannot continue",
        _ => "Something went wrong",
    }
    .into()
}

pub fn failure_from(e: &NyError) -> Failure {
    Failure { title: title_for(e.kind), message: e.message.clone(), hint: e.hint.clone(), kind: Some(e.kind) }
}


/// Turns the engine's jumpy instant speed (drives write in bursts) into numbers that read calmly: the speed is the
/// average over the last ~15 seconds, blended with what was shown before, and both it and the time left are
/// recalculated only once a second. The time left is rounded to a step that grows with its size.
#[derive(Default)]
pub struct Smoother {
    samples: std::collections::VecDeque<(std::time::Instant, u64)>,
    shown_at: Option<std::time::Instant>,
    speed: f64,
    eta: Option<f64>,
}

impl Smoother {
    const WINDOW: std::time::Duration = std::time::Duration::from_secs(15);

    pub fn feed(&mut self, bytes_done: u64, total: Option<u64>) {
        let now = std::time::Instant::now();
        self.samples.push_back((now, bytes_done));
        while self.samples.len() > 2 && now.duration_since(self.samples[1].0) > Self::WINDOW {
            self.samples.pop_front();
        }
        if self.shown_at.is_some_and(|t| now.duration_since(t).as_secs_f64() < 1.0) {
            return;
        }
        let (t0, b0) = self.samples[0];
        let dt = now.duration_since(t0).as_secs_f64();
        if dt < 2.0 || bytes_done < b0 {
            return; // not enough to say anything yet
        }
        let fresh = (bytes_done - b0) as f64 / dt;
        self.speed = if self.speed > 0.0 { 0.7 * self.speed + 0.3 * fresh } else { fresh };
        self.shown_at = Some(now);
        self.eta = match total {
            Some(t) if t > bytes_done && self.speed > 1.0 => Some(round_eta((t - bytes_done) as f64 / self.speed)),
            _ => None,
        };
    }

    /// Bytes per second, or None while there is not enough data.
    pub fn speed(&self) -> Option<f64> {
        (self.shown_at.is_some() && self.speed > 0.0).then_some(self.speed)
    }

    pub fn eta(&self) -> Option<f64> {
        self.eta
    }
}

fn round_eta(secs: f64) -> f64 {
    let step = if secs < 20.0 { 1.0 } else if secs < 120.0 { 5.0 } else if secs < 3600.0 { 15.0 } else { 60.0 };
    (secs / step).round().max(1.0) * step
}
