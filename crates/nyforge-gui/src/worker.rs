//! Runs the NyForge engine (`nyforge`) for the window.
//!
//! The window never writes to a drive itself. It starts `nyforge … --json` as a child process, reads the
//! JSON event stream, and cancels by closing the child's standard input (or, on macOS, by creating a file the
//! engine watches). Whatever the engine reports is what the window shows.

use crate::elevation::{self, Privilege};
use eframe::egui;
use nyforge_core::progress::Event;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

pub enum Msg {
    Event(Event),
    /// Any other JSON line (the NyBoot file listing, the format report).
    Json(serde_json::Value),
    /// A line the engine printed to its error stream (kept for diagnostics).
    Stderr(String),
    Exit(Option<i32>),
    SpawnFailed(String),
}

enum Canceller {
    Stdin(Option<ChildStdin>),
    File(PathBuf),
    None,
}

pub struct Job {
    pub rx: Receiver<Msg>,
    canceller: Canceller,
    #[allow(dead_code)] // kept for diagnostics
    pub started: Instant,
    pub cancel_requested: bool,
}

impl Job {
    /// Ask the engine to stop. It finishes cleanly and reports "cancelled".
    pub fn cancel(&mut self) {
        self.cancel_requested = true;
        match &mut self.canceller {
            Canceller::Stdin(s) => {
                s.take();
            }
            Canceller::File(p) => {
                let _ = std::fs::write(&p, b"stop");
            }
            Canceller::None => {}
        }
    }
}

impl Job {
    /// Ask the engine to read the drive back when it has finished writing (the engine honours this until it
    /// has finished writing). Over standard input this is the byte `v`; with a file-watching engine (macOS) it is
    /// a second file next to the cancel file.
    pub fn request_verify(&mut self) {
        match &mut self.canceller {
            Canceller::Stdin(Some(s)) => {
                use std::io::Write;
                let _ = s.write_all(b"v");
                let _ = s.flush();
            }
            Canceller::File(p) => {
                let mut v = p.as_os_str().to_owned();
                v.push(".verify");
                let _ = std::fs::write(PathBuf::from(v), b"verify");
            }
            _ => {}
        }
    }
}

pub fn engine_path() -> PathBuf {
    if let Ok(p) = std::env::var("NYFORGE_BIN") {
        return PathBuf::from(p);
    }
    let name = if cfg!(windows) { "nyforge.exe" } else { "nyforge" };
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join(name);
            if p.exists() {
                return p;
            }
        }
    }
    PathBuf::from(name)
}

pub fn engine_available() -> bool {
    let p = engine_path();
    if p.components().count() > 1 {
        return p.is_file();
    }
    std::env::var_os("PATH").map(|path| std::env::split_paths(&path).any(|d| d.join(&p).is_file())).unwrap_or(false)
}

fn use_prompt() -> bool {
    elevation::privilege() == Privilege::AskPerTask && std::env::var_os("NYFORGE_GUI_NO_ELEVATE").is_none()
}

/// Start the engine with these arguments (`--json` is added here).
pub fn spawn(args: Vec<String>, ctx: egui::Context) -> Job {
    spawn_with(args, ctx, true)
}

/// Like `spawn`, but never asks for administrator access (for jobs that only read the person's own files).
pub fn spawn_as_user(args: Vec<String>, ctx: egui::Context) -> Job {
    spawn_with(args, ctx, false)
}

fn spawn_with(args: Vec<String>, ctx: egui::Context, elevate: bool) -> Job {
    let (tx, rx) = channel::<Msg>();
    let engine = engine_path();
    let fail = |tx: &Sender<Msg>, ctx: &egui::Context, why: String| {
        let _ = tx.send(Msg::SpawnFailed(why));
        ctx.request_repaint();
    };
    let use_prompt = || elevate && use_prompt();
    if use_prompt() && cfg!(target_os = "macos") {
        return spawn_macos(&engine, &args, tx, rx, ctx);
    }
    let prompt_linux = use_prompt() && cfg!(target_os = "linux");
    let mut cmd;
    if prompt_linux {
        cmd = Command::new("pkexec");
        cmd.arg(&engine);
    } else {
        cmd = Command::new(&engine);
    }
    cmd.args(&args).arg("--json").arg("--cancel-on-stdin-close");
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let what = if prompt_linux && e.kind() == std::io::ErrorKind::NotFound {
                "Administrator access needs `pkexec` (polkit), which is not installed. Install polkit, or start NyForge from a terminal with sudo.".to_string()
            } else {
                format!("Could not start the NyForge engine ({}): {e}", engine.display())
            };
            fail(&tx, &ctx, what);
            return Job { rx, canceller: Canceller::None, started: Instant::now(), cancel_requested: false };
        }
    };
    let stdin = child.stdin.take();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    let (t1, c1) = (tx.clone(), ctx.clone());
    let out = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            route_line(&line, &t1);
            c1.request_repaint();
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
        // Let the readers drain the pipes before announcing the exit.
        let _ = out.join();
        let _ = err.join();
        let _ = tx.send(Msg::Exit(code));
        ctx.request_repaint();
    });
    Job { rx, canceller: Canceller::Stdin(stdin), started: Instant::now(), cancel_requested: false }
}

fn route_line(line: &str, tx: &Sender<Msg>) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    match serde_json::from_str::<Event>(line) {
        Ok(ev) => {
            let _ = tx.send(Msg::Event(ev));
        }
        Err(_) => match serde_json::from_str::<serde_json::Value>(line) {
            Ok(v) => {
                let _ = tx.send(Msg::Json(v));
            }
            Err(_) => {
                let _ = tx.send(Msg::Stderr(line.to_string()));
            }
        },
    }
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn applescript_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// macOS: `osascript` shows the system administrator prompt and can only hand back an exit code, so the engine
/// runs in the background writing its events to files that this thread follows. Cancel = create a file the engine watches.
fn spawn_macos(engine: &Path, args: &[String], tx: Sender<Msg>, rx: Receiver<Msg>, ctx: egui::Context) -> Job {
    let dir = std::env::temp_dir().join(format!("nyforge-{}-{}", std::process::id(), Instant::now().elapsed().subsec_nanos() ^ rand_bits()));
    let made = std::fs::create_dir(&dir);
    #[cfg(unix)]
    if made.is_ok() {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    if let Err(e) = made {
        let _ = tx.send(Msg::SpawnFailed(format!("Could not create a working folder: {e}")));
        ctx.request_repaint();
        return Job { rx, canceller: Canceller::None, started: Instant::now(), cancel_requested: false };
    }
    let (out, err, rc, cancel) = (dir.join("out"), dir.join("err"), dir.join("rc"), dir.join("cancel"));
    let mut words = vec![sh_quote(&engine.to_string_lossy())];
    words.extend(args.iter().map(|a| sh_quote(a)));
    words.push("--json".into());
    words.push("--cancel-file".into());
    words.push(sh_quote(&cancel.to_string_lossy()));
    // `set -C` makes every `>` refuse to overwrite an existing file or link.
    let script = format!(
        "set -C; umask 022; ( {} > {} 2> {}; echo $? > {} ) > /dev/null 2>&1 &",
        words.join(" "),
        sh_quote(&out.to_string_lossy()),
        sh_quote(&err.to_string_lossy()),
        sh_quote(&rc.to_string_lossy())
    );
    let osa = format!("do shell script {} with administrator privileges", applescript_quote(&script));
    let launched = Command::new("osascript").arg("-e").arg(&osa).stdout(Stdio::null()).stderr(Stdio::piped()).output();
    match launched {
        Ok(o) if o.status.success() => {}
        Ok(o) => {
            let msg = String::from_utf8_lossy(&o.stderr).to_string();
            let why = if msg.contains("-128") || msg.to_lowercase().contains("cancel") {
                "The administrator prompt was cancelled, so nothing was started.".to_string()
            } else {
                format!("macOS refused to start the helper: {}", msg.trim())
            };
            let _ = tx.send(Msg::SpawnFailed(why));
            ctx.request_repaint();
            let _ = std::fs::remove_dir_all(&dir);
            return Job { rx, canceller: Canceller::None, started: Instant::now(), cancel_requested: false };
        }
        Err(e) => {
            let _ = tx.send(Msg::SpawnFailed(format!("Could not run osascript: {e}")));
            ctx.request_repaint();
            let _ = std::fs::remove_dir_all(&dir);
            return Job { rx, canceller: Canceller::None, started: Instant::now(), cancel_requested: false };
        }
    }
    std::thread::spawn(move || {
        use std::io::{Read, Seek, SeekFrom};
        let mut pos = 0u64;
        let mut carry = String::new();
        let mut finished = false;
        loop {
            let done = rc.exists();
            if let Ok(mut f) = std::fs::File::open(&out) {
                if f.seek(SeekFrom::Start(pos)).is_ok() {
                    let mut buf = String::new();
                    if let Ok(n) = f.read_to_string(&mut buf) {
                        pos += n as u64;
                        carry.push_str(&buf);
                        while let Some(i) = carry.find('\n') {
                            let line: String = carry.drain(..=i).collect();
                            route_line(&line, &tx);
                            ctx.request_repaint();
                        }
                    }
                }
            }
            if finished {
                break;
            }
            if done {
                finished = true; // one more pass to drain what was written just before the exit code
                continue;
            }
            std::thread::sleep(Duration::from_millis(120));
        }
        if !carry.trim().is_empty() {
            route_line(&carry, &tx);
        }
        if let Ok(e) = std::fs::read_to_string(&err) {
            for l in e.lines() {
                let _ = tx.send(Msg::Stderr(l.to_string()));
            }
        }
        let code = std::fs::read_to_string(&rc).ok().and_then(|s| s.trim().parse::<i32>().ok());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = tx.send(Msg::Exit(code));
        ctx.request_repaint();
    });
    Job { rx, canceller: Canceller::File(cancel), started: Instant::now(), cancel_requested: false }
}

fn rand_bits() -> u32 {
    let mut b = [0u8; 4];
    let _ = std::fs::File::open("/dev/urandom").and_then(|mut f| std::io::Read::read_exact(&mut f, &mut b));
    u32::from_le_bytes(b)
}
