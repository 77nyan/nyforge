//! Runs the NyForge engine (`nyforge … --json`) as a child process and follows what it reports.
//! The terminal app never writes to a drive itself.

use nyforge_core::error::{ErrorKind, NyError};
use nyforge_core::progress::{Event, Phase, Progress};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

pub enum Msg {
    Event(Event),
    Json(serde_json::Value),
    Stderr(String),
    Exit(Option<i32>),
    SpawnFailed(String),
}

pub fn engine_path() -> PathBuf {
    if let Ok(p) = std::env::var("NYFORGE_BIN") {
        return PathBuf::from(p);
    }
    let name = if cfg!(windows) { "nyforge.exe" } else { "nyforge" };
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.to_path_buf())) {
        let p = dir.join(name);
        if p.exists() {
            return p;
        }
    }
    PathBuf::from(name)
}

pub fn engine_found() -> bool {
    let p = engine_path();
    if p.components().count() > 1 {
        return p.is_file();
    }
    std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|d| d.join(&p).is_file()))
}

pub fn is_root() -> bool {
    nyforge_core::sys::is_root()
}

/// A command that needs `sudo` here (not root, not Windows).
pub fn needs_sudo() -> bool {
    cfg!(unix) && !is_root() && std::env::var_os("NYFORGE_TUI_NO_SUDO").is_none()
}

pub struct Job {
    rx: Receiver<Msg>,
    stdin: Option<ChildStdin>,
}

impl Job {
    /// Closing the engine's standard input asks it to stop; it finishes cleanly and reports "cancelled".
    pub fn cancel(&mut self) {
        self.stdin.take();
    }
}

pub fn spawn(args: &[String], sudo: bool) -> Job {
    let (tx, rx) = channel::<Msg>();
    let engine = engine_path();
    let mut cmd;
    if sudo {
        cmd = Command::new("sudo");
        cmd.arg("-n").arg(&engine);
    } else {
        cmd = Command::new(&engine);
    }
    cmd.args(args).arg("--json").arg("--cancel-on-stdin-close");
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(Msg::SpawnFailed(format!("Could not start the NyForge engine ({}): {e}", engine.display())));
            return Job { rx, stdin: None };
        }
    };
    let stdin = child.stdin.take();
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    let t1 = tx.clone();
    let out = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            route(&line, &t1);
        }
    });
    let t2 = tx.clone();
    let err = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = t2.send(Msg::Stderr(line));
        }
    });
    std::thread::spawn(move || {
        let code = child.wait().ok().and_then(|s| s.code());
        let _ = out.join();
        let _ = err.join();
        let _ = tx.send(Msg::Exit(code));
    });
    Job { rx, stdin }
}

fn route(line: &str, tx: &std::sync::mpsc::Sender<Msg>) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    let _ = match serde_json::from_str::<Event>(line) {
        Ok(ev) => tx.send(Msg::Event(ev)),
        Err(_) => match serde_json::from_str::<serde_json::Value>(line) {
            Ok(v) => tx.send(Msg::Json(v)),
            Err(_) => tx.send(Msg::Stderr(line.to_string())),
        },
    };
}

pub enum Outcome {
    Running,
    Success,
    Cancelled,
    Failed(String, Option<String>),
}

pub struct Run {
    pub job: Job,
    pub phase: Option<(Phase, String)>,
    pub progress: Option<Progress>,
    pub total_hint: Option<u64>,
    pub finished: Option<(f64, f64)>,
    pub verified: Option<(f64, f64)>,
    pub verified_note: Option<String>,
    pub smooth: Smoother,
    pub error: Option<NyError>,
    pub target_error: Option<NyError>,
    pub stderr: Vec<String>,
    pub json: Vec<serde_json::Value>,
    exit: Option<Option<i32>>,
    spawn_failed: Option<String>,
    needs_finished: bool,
    pub started: Instant,
}

impl Run {
    pub fn new(job: Job, needs_finished: bool) -> Self {
        Run {
            job,
            phase: None,
            progress: None,
            total_hint: None,
            finished: None,
            verified: None,
            verified_note: None,
            smooth: Smoother::default(),
            error: None,
            target_error: None,
            stderr: vec![],
            json: vec![],
            exit: None,
            spawn_failed: None,
            needs_finished,
            started: Instant::now(),
        }
    }

    /// Reads everything the engine has said so far. True if anything arrived.
    pub fn pump(&mut self) -> bool {
        let mut any = false;
        while let Ok(m) = self.job.rx.try_recv() {
            any = true;
            match m {
                Msg::Event(ev) => match ev {
                    Event::Start { total_bytes, .. } => self.total_hint = total_bytes,
                    Event::Phase { phase, detail } => {
                        if phase == Phase::Verifying && !matches!(self.phase, Some((Phase::Verifying, _))) {
                            self.smooth = Smoother::default();
                            self.progress = None;
                        }
                        self.phase = Some((phase, detail));
                    }
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
                    Event::Verified { elapsed_secs, avg_speed_bps, note, .. } => {
                        self.verified = Some((elapsed_secs, avg_speed_bps));
                        self.verified_note = note;
                    }
                    Event::Error { error } => self.error = Some(error),
                    Event::Log { .. } => {}
                },
                Msg::Json(v) => self.json.push(v),
                Msg::Stderr(l) => {
                    self.stderr.push(l);
                    if self.stderr.len() > 20 {
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

    pub fn is_verifying(&self) -> bool {
        matches!(self.phase, Some((Phase::Verifying, _)))
    }

    pub fn find_json(&self, key: &str) -> Option<&serde_json::Value> {
        self.json.iter().find(|v| v.get(key).is_some())
    }

    pub fn fraction(&self) -> Option<f64> {
        self.progress.as_ref().map(|p| p.fraction.clamp(0.0, 1.0))
    }

    pub fn outcome(&self) -> Outcome {
        if let Some(w) = &self.spawn_failed {
            return Outcome::Failed(w.clone(), None);
        }
        let Some(code) = self.exit else { return Outcome::Running };
        if matches!(&self.error, Some(e) if e.kind == ErrorKind::Cancelled) || code == Some(130) {
            return Outcome::Cancelled;
        }
        if code == Some(0) && self.error.is_none() && self.target_error.is_none() && (!self.needs_finished || self.finished.is_some()) {
            return Outcome::Success;
        }
        if let Some(e) = self.error.as_ref().or(self.target_error.as_ref()) {
            return Outcome::Failed(e.message.clone(), e.hint.clone());
        }
        let tail = self.stderr.iter().rev().take(3).rev().cloned().collect::<Vec<_>>().join(" ");
        if code == Some(1) && tail.contains("password is required") {
            return Outcome::Failed("Administrator access was not granted, so nothing was changed.".into(), Some("Run nyforge -t with sudo, or try again.".into()));
        }
        Outcome::Failed(
            if tail.is_empty() {
                match code {
                    Some(c) => format!("The engine stopped unexpectedly (exit code {c})."),
                    None => "The engine was stopped unexpectedly.".into(),
                }
            } else {
                tail
            },
            None,
        )
    }
}

/// Turns the engine's jumpy instant speed into numbers that read calmly: an average over ~15 s, refreshed once a second.
#[derive(Default)]
pub struct Smoother {
    samples: std::collections::VecDeque<(Instant, u64)>,
    shown_at: Option<Instant>,
    speed: f64,
    eta: Option<f64>,
}

impl Smoother {
    const WINDOW: Duration = Duration::from_secs(15);

    pub fn feed(&mut self, done: u64, total: Option<u64>) {
        let now = Instant::now();
        self.samples.push_back((now, done));
        while self.samples.len() > 2 && now.duration_since(self.samples[1].0) > Self::WINDOW {
            self.samples.pop_front();
        }
        if self.shown_at.is_some_and(|t| now.duration_since(t).as_secs_f64() < 1.0) {
            return;
        }
        let (t0, b0) = self.samples[0];
        let dt = now.duration_since(t0).as_secs_f64();
        if dt < 2.0 || done < b0 {
            return;
        }
        let fresh = (done - b0) as f64 / dt;
        self.speed = if self.speed > 0.0 { 0.7 * self.speed + 0.3 * fresh } else { fresh };
        self.shown_at = Some(now);
        self.eta = match total {
            Some(t) if t > done && self.speed > 1.0 => Some(round_eta((t - done) as f64 / self.speed)),
            _ => None,
        };
    }

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
