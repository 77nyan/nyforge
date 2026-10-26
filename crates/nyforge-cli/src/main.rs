//! `nyforge` — the command line front end.
//!
//! Human output goes to stderr (progress) and stdout (results); with `--json`
//! stdout carries one JSON event per line and nothing else, so scripts and any
//! future front end can follow along.

mod boot;
mod format_cmd;
mod setup;
mod ui;
mod verify_cmd;
mod windows;

use clap::{Args, Parser, Subcommand};
use nyforge_core::clone::{clone_to_file, Compression};
use nyforge_core::device::{self, Drive, ResolvedTarget, Safety, TargetPolicy};
use nyforge_core::error::{ErrorKind, NyError};
use nyforge_core::format::{human_size, human_speed};
use nyforge_core::image;
use nyforge_core::progress::Event;
use nyforge_core::usbformat;
use nyforge_core::writer::{write_image, Target, WriteOptions};
use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use ui::Ui;

#[derive(Parser)]
#[command(
    name = "nyforge",
    version = concat!(env!("CARGO_PKG_VERSION"), " (", env!("NYFORGE_BUILD"), ")"),
    about = "Fast, safe disk image flasher",
    long_about = "NyForge writes disk images (.img, .iso, and .gz/.xz/.bz2/.zst/.zip archives of them, or http(s) URLs) \
                  to USB drives and SD cards. It never offers the drive your system runs from."
)]
struct Cli {
    /// (for front ends) treat the end of standard input as a request to cancel
    #[arg(long, global = true, hide = true)]
    cancel_on_stdin_close: bool,
    /// (for front ends that cannot hold our standard input) cancel as soon as this file exists
    #[arg(long, global = true, hide = true, value_name = "FILE")]
    cancel_file: Option<std::path::PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show the drives NyForge can write to
    List(ListArgs),
    /// Look at an image without writing it
    Inspect(InspectArgs),
    /// Write an image to one or more drives
    Flash(FlashArgs),
    /// Copy a drive into an image file (compressed if the name ends in .gz, .zst or .xz)
    Clone(CloneArgs),
    /// Set host name, user, SSH and Wi-Fi on a Raspberry Pi style image that is already on a drive
    Setup(setup::SetupCmd),
    /// Make a bootable Windows installer from a Windows ISO (splits install.wim for FAT32)
    Windows(windows::WindowsArgs),
    /// Read a drive back and check that it holds exactly what was written to it
    Verify(verify_cmd::VerifyArgs),
    /// Check a download against its published SHA-256, SHA-1 or MD5
    Checksum(verify_cmd::ChecksumArgs),
    /// NyBoot: a multiboot drive you copy ISOs onto and boot any of them
    #[command(subcommand)]
    Boot(boot::BootCmd),
    /// Erase and format a USB drive or SD card (FAT32, exFAT, NTFS, ext4, GPT/MBR, wipe, fake-capacity test). Never touches PC drives.
    #[command(visible_alias = "formatusb")]
    Format(format_cmd::FormatArgs),
    /// Check that this machine can run NyForge properly
    Doctor(JsonOnly),
}

#[derive(Args)]
struct ListArgs {
    /// Also show internal, virtual and system drives (they stay protected)
    #[arg(long)]
    all: bool,
    /// Machine-readable output
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct JsonOnly {
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct InspectArgs {
    /// Image file or http(s) URL
    image: String,
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct FlashArgs {
    /// Image file or http(s) URL
    image: String,
    /// Drive(s) to write (e.g. /dev/sdb, /dev/disk4, \\.\PhysicalDrive2), or a file path to write an image file
    #[arg(required = true, num_args = 1..)]
    targets: Vec<String>,
    /// Do not ask for confirmation (required when not run from a terminal)
    #[arg(short = 'y', long)]
    yes: bool,
    /// Allow fixed internal disks (never the system disk)
    #[arg(long)]
    allow_internal: bool,
    /// Allow loop/virtual devices
    #[arg(long)]
    allow_virtual: bool,
    /// One JSON event per line on stdout instead of the progress bar
    #[arg(long)]
    json: bool,
    /// Fail instead of unmounting the drive's mounted partitions
    #[arg(long)]
    no_unmount: bool,
    /// Send zero blocks too instead of skipping them
    #[arg(long)]
    no_zero_skip: bool,
    /// Leave the end of the drive untouched
    #[arg(long)]
    no_wipe_tail: bool,
    /// Buffered I/O instead of direct I/O
    #[arg(long)]
    no_direct: bool,
    /// Chunk size in MiB (default 4)
    #[arg(long, value_name = "MIB")]
    chunk: Option<usize>,
    /// When the write is done, read the drive back and compare it with the image (needs a local file, not a URL)
    #[arg(long)]
    verify: bool,
    #[command(flatten)]
    setup: setup::SetupArgs,
}

#[derive(Args)]
struct CloneArgs {
    /// Drive to read, e.g. /dev/sdb
    drive: String,
    /// Image file to create
    output: String,
    /// Replace the output file if it exists
    #[arg(long)]
    force: bool,
    #[arg(long)]
    allow_internal: bool,
    #[arg(long)]
    allow_virtual: bool,
    #[arg(long)]
    json: bool,
}

pub(crate) const EXIT_FAIL: u8 = 1;
const EXIT_PARTIAL: u8 = 2;
const EXIT_CANCEL: u8 = 130;

fn main() -> ExitCode {
    #[cfg(unix)]
    unsafe {
        // Behave like other CLI tools when piped into `head`: stop quietly instead of panicking.
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let cli = Cli::parse();
    CANCEL_ON_STDIN_CLOSE.store(cli.cancel_on_stdin_close, Ordering::Relaxed);
    if let Some(p) = cli.cancel_file.clone() {
        let _ = CANCEL_FILE.set(p);
    }
    let json = match &cli.cmd {
        Cmd::List(a) => a.json,
        Cmd::Inspect(a) => a.json,
        Cmd::Flash(a) => a.json,
        Cmd::Clone(a) => a.json,
        Cmd::Doctor(a) => a.json,
        Cmd::Boot(b) => boot_json(b),
        Cmd::Windows(a) => a.json,
        Cmd::Verify(a) => a.json,
        Cmd::Checksum(a) => a.json,
        Cmd::Setup(a) => a.json,
        Cmd::Format(a) => a.json,
    };
    let r = match cli.cmd {
        Cmd::List(a) => cmd_list(a),
        Cmd::Inspect(a) => cmd_inspect(a),
        Cmd::Flash(a) => cmd_flash(a),
        Cmd::Clone(a) => cmd_clone(a),
        Cmd::Doctor(a) => cmd_doctor(a),
        Cmd::Boot(b) => boot::run(b),
        Cmd::Windows(a) => windows::run(a),
        Cmd::Verify(a) => verify_cmd::run(a),
        Cmd::Checksum(a) => verify_cmd::run_checksum(a),
        Cmd::Setup(a) => setup::run(a),
        Cmd::Format(a) => format_cmd::run(a),
    };
    match r {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            if json {
                if !ERROR_PRINTED.load(Ordering::Relaxed) {
                    crate::print_event(&Event::Error { error: e.clone() });
                }
            } else {
                Ui::print_error(&e);
            }
            ExitCode::from(if e.kind == ErrorKind::Cancelled { EXIT_CANCEL } else { EXIT_FAIL })
        }
    }
}

pub(crate) type Res = Result<u8, NyError>;

fn boot_json(b: &boot::BootCmd) -> bool {
    use boot::BootCmd::*;
    match b {
        Install(a) => a.common.json,
        Add(a) => a.common.json,
        Ls(a) => a.common.json,
        Rm(a) => a.common.json,
        Check(a) => a.json,
    }
}

// ----------------------------------------------------------------------- list

fn cmd_list(a: ListArgs) -> Res {
    let drives = device::list_drives()?;
    let shown: Vec<&Drive> = drives.iter().filter(|d| a.all || (d.safety == Safety::Safe && d.size > 0)).collect();
    if a.json {
        println!("{}", serde_json::to_string_pretty(&shown).unwrap());
        return Ok(0);
    }
    if shown.is_empty() {
        println!("No removable drives found.");
        if !a.all {
            println!("Plug one in, or use --all to see every drive (internal ones stay protected).");
        }
        return Ok(0);
    }
    let ui = Ui::new();
    let mut rows: Vec<Vec<String>> = vec![vec!["DRIVE".into(), "SIZE".into(), "TYPE".into(), "NAME".into(), "CONTAINS".into(), "STATUS".into()]];
    for d in &shown {
        let status = match d.safety {
            Safety::Safe if d.read_only => "read-only",
            Safety::Safe => "ready",
            Safety::Internal => "internal (needs --allow-internal)",
            Safety::Virtual => "virtual (needs --allow-virtual)",
            Safety::System => "SYSTEM — protected",
        };
        rows.push(vec![d.path.clone(), human_size(d.size), d.bus.label().into(), d.display_name(), d.contents_summary(), status.into()]);
    }
    ui.table(&rows);
    Ok(0)
}

// -------------------------------------------------------------------- inspect

fn cmd_inspect(a: InspectArgs) -> Res {
    let info = if image::is_url(&a.image) { image::open(&a.image)?.info } else { image::describe_file(Path::new(&a.image))? };
    if a.json {
        println!("{}", serde_json::to_string_pretty(&info).unwrap());
        return Ok(0);
    }
    println!("Image:        {}", info.label);
    println!("Format:       {}", info.container.name());
    if let Some(n) = &info.inner_name {
        println!("Inside:       {n}");
    }
    if let Some(c) = info.compressed_size {
        println!("File size:    {}", human_size(c));
    }
    match info.decompressed_size {
        Some(d) => println!("Written size: {}", human_size(d)),
        None => println!("Written size: unknown until it is unpacked"),
    }
    Ok(0)
}

// ---------------------------------------------------------------------- flash

/// Root (Linux, macOS) or an elevated Administrator (Windows).
pub(crate) fn is_root() -> bool {
    nyforge_core::sys::is_root()
}

/// How to get the rights this command needs, in the words of this platform.
pub(crate) fn rerun_hint(cmd: &str) -> String {
    if cfg!(windows) {
        nyforge_core::sys::elevation_hint().to_string()
    } else {
        format!("Run it again with: sudo {cmd}")
    }
}

static ERROR_PRINTED: AtomicBool = AtomicBool::new(false);

/// One JSON event on stdout. The final error is printed once, even when the engine already reported it.
pub(crate) fn print_event(ev: &Event) {
    if matches!(ev, Event::Error { .. }) {
        ERROR_PRINTED.store(true, Ordering::Relaxed);
    }
    println!("{}", serde_json::to_string(ev).unwrap());
}

static CANCEL_ON_STDIN_CLOSE: AtomicBool = AtomicBool::new(false);
/// A front end asked, while we were writing, for the drive to be read back afterwards.
static VERIFY_REQUESTED: AtomicBool = AtomicBool::new(false);

pub(crate) fn verify_requested() -> bool {
    VERIFY_REQUESTED.load(Ordering::Relaxed)
}
static CANCEL_FILE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

pub(crate) fn install_ctrlc() -> Arc<AtomicBool> {
    let cancel = Arc::new(AtomicBool::new(false));
    if CANCEL_ON_STDIN_CLOSE.load(Ordering::Relaxed) {
        // A front end that starts us (possibly through pkexec, which we cannot signal) closes our
        // standard input to say "stop".
        let c = cancel.clone();
        std::thread::spawn(move || {
            let mut sink = [0u8; 256];
            let mut stdin = std::io::stdin().lock();
            loop {
                match std::io::Read::read(&mut stdin, &mut sink) {
                    Ok(0) | Err(_) => break,
                    // The byte `v` means "read the drive back when you are done writing".
                    Ok(n) => {
                        if sink[..n].contains(&b'v') {
                            VERIFY_REQUESTED.store(true, Ordering::SeqCst);
                        }
                    }
                }
            }
            c.store(true, Ordering::SeqCst);
        });
    }
    if let Some(path) = CANCEL_FILE.get().cloned() {
        // macOS: the administrator prompt cannot give the window a live pipe to us, so it creates this file instead.
        let c = cancel.clone();
        let mut verify_file = path.as_os_str().to_owned();
        verify_file.push(".verify");
        let verify_file = std::path::PathBuf::from(verify_file);
        std::thread::spawn(move || loop {
            if path.exists() {
                c.store(true, Ordering::SeqCst);
                break;
            }
            if verify_file.exists() {
                VERIFY_REQUESTED.store(true, Ordering::SeqCst);
            }
            std::thread::sleep(std::time::Duration::from_millis(150));
        });
    }
    let c = cancel.clone();
    let _ = ctrlc::set_handler(move || {
        if c.swap(true, Ordering::SeqCst) {
            // Second Ctrl-C: the user really wants out.
            std::process::exit(EXIT_CANCEL as i32);
        }
    });
    cancel
}

fn resolve_all(specs: &[String], policy: TargetPolicy) -> Result<Vec<ResolvedTarget>, NyError> {
    specs.iter().map(|s| device::resolve_target(s, policy)).collect()
}

pub(crate) fn short_name(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

pub(crate) fn confirm_erase(drives: &[&Drive], image_label: &str, ui: &Ui) -> Result<(), NyError> {
    if !std::io::stdin().is_terminal() {
        return Err(NyError::new(ErrorKind::InvalidInput, "Refusing to erase a drive without confirmation.")
            .with_hint("Run it from a terminal, or add --yes if you are sure."));
    }
    eprintln!();
    for d in drives {
        eprintln!(
            "  {}  {}  {}  [{}]",
            ui.bold(&d.path),
            human_size(d.size),
            d.display_name(),
            d.contents_summary()
        );
    }
    eprintln!();
    eprintln!("{} {}", ui.red("Everything on the drive(s) above will be erased and replaced with"), ui.bold(image_label));
    let word = if drives.len() == 1 { short_name(&drives[0].path) } else { "erase".to_string() };
    eprint!("Type {} to continue: ", ui.bold(&word));
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).ok();
    if line.trim() == word {
        Ok(())
    } else {
        Err(NyError::cancelled().with_hint("Nothing was written."))
    }
}

fn cmd_flash(a: FlashArgs) -> Res {
    let policy = TargetPolicy { allow_internal: a.allow_internal, allow_virtual: a.allow_virtual };
    let ui = Ui::new();

    // Check the first-boot options before anything is erased.
    let firstboot = if a.setup.requested() { Some(a.setup.to_firstboot()?) } else { None };
    if a.verify && image::is_url(&a.image) {
        return Err(NyError::invalid("--verify needs the image as a file on this computer, not a URL.")
            .with_hint("Download it first (and check it with `nyforge checksum`), then flash the file."));
    }
    let source = image::open(&a.image)?;
    let resolved = resolve_all(&a.targets, policy)?;

    let drives: Vec<&Drive> = resolved
        .iter()
        .filter_map(|r| if let ResolvedTarget::Device(d) = r { Some(d) } else { None })
        .collect();
    if !drives.is_empty() && !is_root() {
        return Err(NyError::new(ErrorKind::Permission, "Writing to a drive needs administrator rights.")
            .with_hint(rerun_hint(&format!("nyforge flash {} {}", a.image, a.targets.join(" ")))));
    }
    if let Some(size) = source.info.decompressed_size {
        for d in &drives {
            if size > d.size {
                return Err(NyError::new(
                    ErrorKind::NoSpace,
                    format!("The image ({}) does not fit on {} ({}).", human_size(size), d.path, human_size(d.size)),
                )
                .with_hint("Pick a bigger drive."));
            }
        }
    }

    if !drives.is_empty() && !a.yes {
        if a.json {
            return Err(NyError::new(ErrorKind::InvalidInput, "--json needs --yes: there is nobody to ask.")
                .with_hint("Add --yes once you have checked the target."));
        }
        confirm_erase(&drives, &source.info.label, &ui)?;
    }

    let touches_drive = !drives.is_empty();
    let mut opts = WriteOptions::default();
    opts.unmount = !a.no_unmount;
    opts.skip_zeros = !a.no_zero_skip;
    opts.wipe_tail = !a.no_wipe_tail;
    opts.direct_io = !a.no_direct;
    if let Some(m) = a.chunk {
        opts.chunk_size = m.clamp(1, 64) << 20;
    }

    let targets: Vec<Target> = resolved.into_iter().map(Target::from_resolved).collect();
    let setup_targets = targets.clone();
    let cancel = install_ctrlc();
    let mut renderer = Ui::new();
    let json = a.json;
    let mut emit = |ev: Event| {
        if json {
            crate::print_event(&ev);
        } else {
            renderer.render(&ev);
        }
    };
    let res = write_image(source, targets, &opts, cancel.clone(), &mut emit);
    match res {
        Ok(report) => {
            if (a.verify || verify_requested()) && report.all_ok() {
                renderer.finish_line();
                for t in report.targets.iter().filter(|t| t.ok) {
                    let Some(tg) = setup_targets.iter().find(|x| x.display == t.target) else { continue };
                    verify_cmd::verify_after(&a.image, tg, opts.direct_io, json, &mut renderer, cancel.clone())?;
                }
            }
            if !json {
                renderer.finish_line();
                let ok = report.targets.iter().filter(|t| t.ok).count();
                if report.all_ok() {
                    let secs = report.elapsed.as_secs_f64();
                    eprintln!(
                        "{} Wrote {} to {} in {} ({}). {}",
                        renderer.green("✔"),
                        human_size(report.image_bytes),
                        if report.targets.len() == 1 { report.targets[0].target.clone() } else { format!("{} drives", ok) },
                        nyforge_core::format::human_duration(secs),
                        human_speed(if secs > 0.0 { report.image_bytes as f64 / secs } else { 0.0 }),
                        if touches_drive { "You can remove the drive now." } else { "" }
                    );
                } else {
                    for t in report.targets.iter().filter(|t| !t.ok) {
                        if let Some(e) = &t.error {
                            eprintln!("{} {}: {}", renderer.red("✘"), t.target, e.message);
                            if let Some(h) = &e.hint {
                                eprintln!("    {h}");
                            }
                        }
                    }
                    eprintln!("{} of {} drives finished.", ok, report.targets.len());
                }
            }
            let mut code = if report.all_ok() { 0 } else { EXIT_PARTIAL };
            if let Some(fb) = &firstboot {
                for t in report.targets.iter().filter(|t| t.ok) {
                    let Some(tg) = setup_targets.iter().find(|x| x.display == t.target) else { continue };
                    match nyforge_core::firstboot::apply(tg, fb, None) {
                        Ok(r) => {
                            if json {
                                println!("{}", serde_json::to_string(&r).unwrap());
                            } else {
                                setup::print_report(&r, &t.target);
                            }
                        }
                        Err(e) => {
                            // The image itself is fine; say what failed and keep going.
                            Ui::print_error(&e);
                            eprintln!("The image was written, but first-boot setup was not applied. Retry with: nyforge setup {} …", t.target);
                            code = EXIT_PARTIAL;
                        }
                    }
                }
            }
            Ok(code)
        }
        Err(e) => {
            renderer.finish_line();
            Err(e)
        }
    }
}

// ---------------------------------------------------------------------- clone

fn cmd_clone(a: CloneArgs) -> Res {
    let policy = TargetPolicy { allow_internal: true, allow_virtual: true };
    let _ = (a.allow_internal, a.allow_virtual);
    let drive = match device::resolve_target(&a.drive, policy)? {
        ResolvedTarget::Device(d) => d,
        ResolvedTarget::File { .. } => {
            return Err(NyError::invalid(format!("{} is not a drive.", a.drive)).with_hint("Run `nyforge list --all` to see drives."))
        }
    };
    if !is_root() {
        return Err(NyError::new(ErrorKind::Permission, "Reading a whole drive needs administrator rights.")
            .with_hint(rerun_hint(&format!("nyforge clone {} {}", a.drive, a.output))));
    }
    let out = Path::new(&a.output);
    if out.exists() && !a.force {
        return Err(NyError::new(ErrorKind::InvalidInput, format!("{} already exists.", a.output)).with_hint("Add --force to replace it."));
    }
    let comp = Compression::from_path(out);
    let cancel = install_ctrlc();
    let mut renderer = Ui::new();
    let json = a.json;
    let mut emit = |ev: Event| {
        if json {
            crate::print_event(&ev);
        } else {
            renderer.render(&ev);
        }
    };
    let label = format!("{} ({})", drive.path, drive.display_name());
    let r = clone_to_file(Path::new(&drive.path), drive.size, &label, out, comp, cancel, &mut emit);
    renderer.finish_line();
    let report = r?;
    if !json {
        eprintln!(
            "{} Saved {} of {} to {} ({}).",
            renderer.green("✔"),
            human_size(report.image_bytes),
            drive.path,
            a.output,
            comp.name()
        );
    }
    Ok(0)
}

// --------------------------------------------------------------------- doctor

fn cmd_doctor(a: JsonOnly) -> Res {
    let drives = device::list_drives();
    let root = is_root();
    let (n_all, n_safe, drive_err) = match &drives {
        Ok(d) => (d.len(), d.iter().filter(|x| x.safety == Safety::Safe).count(), None),
        Err(e) => (0, 0, Some(e.message.clone())),
    };
    if a.json {
        println!(
            "{}",
            serde_json::json!({
                "version": env!("CARGO_PKG_VERSION"),
                "build": env!("NYFORGE_BUILD"),
                "os": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
                "privileged": root,
                "drives_total": n_all,
                "drives_ready": n_safe,
                "drive_detection_error": drive_err,
                "can_format": {
                    "fat16": true, "fat32": true, "exfat": true,
                    "ntfs": usbformat::can_make(usbformat::Fs::Ntfs),
                    "ext4": usbformat::can_make(usbformat::Fs::Ext4),
                },
            })
        );
        return Ok(0);
    }
    let ui = Ui::new();
    let ok = |b: bool| if b { ui.green("✔") } else { ui.red("✘") };
    println!("NyForge {} on {} ({})", env!("CARGO_PKG_VERSION"), std::env::consts::OS, std::env::consts::ARCH);
    println!("{} Drive detection {}", ok(drive_err.is_none()), drive_err.clone().unwrap_or_else(|| format!("works ({n_all} drives, {n_safe} removable)")));
    println!(
        "{} Administrator rights {}",
        ok(root),
        if root {
            "available".to_string()
        } else if cfg!(windows) {
            "missing — flashing needs a terminal opened with \"Run as administrator\"".to_string()
        } else {
            "missing — flashing needs sudo".to_string()
        }
    );
    for fs in [usbformat::Fs::Ext4, usbformat::Fs::Ntfs] {
        match usbformat::fsmake::check_tool(fs) {
            Ok(()) => println!("{} Can format {}", ui.green("✔"), fs.name()),
            Err(e) => println!("{} Cannot format {} ({}) — FAT32, exFAT and FAT16 still work", ui.dim("–"), fs.name(), e.hint.unwrap_or(e.message)),
        }
    }
    println!("{} Network is only used when you give an http(s) image URL", ui.green("✔"));
    Ok(0)
}
