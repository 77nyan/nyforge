//! Finding drives, and deciding which of them a tool may offer.
//!
//! The rules are the engine's own (`nyforge-core`): the window adds no looser ones. A drive is only ever
//! identified by its *identity* (path + model + serial + size), never by its position in a list.

use nyforge_core::device::{self, Bus, Drive, Safety};
use nyforge_core::error::NyError;
use nyforge_core::format::human_size;
use nyforge_core::usbformat;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Purpose {
    Flash,
    Boot,
    Format,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DriveId {
    pub path: String,
    pub vendor: String,
    pub model: String,
    pub serial: Option<String>,
    pub size: u64,
}

impl DriveId {
    pub fn of(d: &Drive) -> Self {
        DriveId { path: d.path.clone(), vendor: d.vendor.clone(), model: d.model.clone(), serial: d.serial.clone(), size: d.size }
    }
}

#[derive(Clone, Debug)]
pub struct Candidate {
    pub id: DriveId,
    pub drive: Drive,
    pub name: String,
    /// Why this drive cannot be chosen (it is still listed, greyed out, so nobody wonders where it went).
    pub blocked: Option<String>,
    /// Things worth knowing before choosing it.
    pub warnings: Vec<String>,
    /// A plain file used as a stand-in for a drive. Only exists when NYFORGE_GUI_TEST_FILES is set (testing).
    pub test_file: bool,
}

impl Candidate {
    pub fn size_text(&self) -> String {
        human_size(self.drive.size)
    }

    /// "USB · removable" as far as the engine can establish it. Never claims more.
    pub fn kind_text(&self) -> String {
        if self.test_file {
            return "Test image file".into();
        }
        let bus = self.drive.bus.label();
        match (self.drive.removable, self.drive.bus) {
            (true, Bus::Usb | Bus::Sd) => format!("{bus} · removable"),
            (true, _) => format!("{bus} · reported removable"),
            (false, _) => format!("{bus} · not reported removable"),
        }
    }

    pub fn is_selectable(&self) -> bool {
        self.blocked.is_none()
    }
}

/// Testing aid: `NYFORGE_GUI_ALLOW_VIRTUAL=1` also lists loop devices (and passes the engine's `--allow-virtual`).
pub fn allow_virtual() -> bool {
    std::env::var_os("NYFORGE_GUI_ALLOW_VIRTUAL").is_some()
}

/// Testing aid: `NYFORGE_GUI_TEST_FILES=/a.img:/b.img` lists existing image files as stand-in drives.
fn test_files() -> Vec<std::path::PathBuf> {
    std::env::var_os("NYFORGE_GUI_TEST_FILES").map(|v| std::env::split_paths(&v).filter(|p| p.is_file()).collect()).unwrap_or_default()
}

fn warnings_for(d: &Drive) -> Vec<String> {
    let mut w = vec![];
    if !matches!(d.bus, Bus::Usb | Bus::Sd) {
        w.push(format!("NyForge cannot confirm this is a USB or SD device (it appears as {}). Check the name and size carefully.", d.bus.label()));
    } else if !d.removable {
        w.push("The system does not report this drive as removable. Check the name and size carefully.".into());
    }
    if d.size == 0 {
        w.push("No media detected.".into());
    }
    w
}

fn candidate(d: Drive, blocked: Option<String>) -> Candidate {
    let warnings = if blocked.is_none() { warnings_for(&d) } else { vec![] };
    Candidate { id: DriveId::of(&d), name: d.display_name(), drive: d, blocked, warnings, test_file: false }
}

fn test_candidate(p: &std::path::Path) -> Option<Candidate> {
    let size = std::fs::metadata(p).ok()?.len();
    let d = Drive {
        path: p.to_string_lossy().into_owned(),
        vendor: String::new(),
        model: p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "image file".into()),
        serial: None,
        size,
        sector_size: 512,
        bus: Bus::Virtual,
        removable: true,
        read_only: false,
        safety: Safety::Virtual,
        partitions: vec![],
    };
    let mut c = candidate(d, None);
    c.test_file = true;
    c.warnings.clear();
    Some(c)
}

/// Every drive this tool may offer (or show greyed out), as the engine sees them right now.
pub fn scan(purpose: Purpose) -> Result<Vec<Candidate>, NyError> {
    let all = device::list_drives()?;
    let mut out = vec![];
    for d in all {
        if d.size == 0 {
            continue; // an empty card reader slot is not a drive
        }
        match purpose {
            Purpose::Format => match usbformat::check_drive_allowed(&d) {
                Ok(()) => out.push(candidate(d, None)),
                Err(e) if e.kind == nyforge_core::ErrorKind::ReadOnly => out.push(candidate(d, Some("Write-protected".into()))),
                Err(_) => {} // internal, system and virtual disks never appear in NyFormat
            },
            Purpose::Flash | Purpose::Boot => match d.safety {
                Safety::Safe if d.read_only => out.push(candidate(d, Some("Write-protected".into()))),
                Safety::Safe => out.push(candidate(d, None)),
                Safety::Virtual if allow_virtual() && purpose != Purpose::Format => out.push(candidate(d, None)),
                _ => {}
            },
        }
    }
    for p in test_files() {
        if let Some(c) = test_candidate(&p) {
            out.push(c);
        }
    }
    out.sort_by(|a, b| a.drive.path.cmp(&b.drive.path));
    Ok(out)
}

/// Look again, right now, and make sure `id` is still the same drive and still allowed. Used immediately
/// before anything destructive starts.
pub fn revalidate(purpose: Purpose, id: &DriveId) -> Result<Candidate, String> {
    let now = scan(purpose).map_err(|e| format!("Could not check the drive again: {}", e.message))?;
    match now.into_iter().find(|c| c.id.path == id.path) {
        None => Err(format!("{} is no longer connected. Plug it back in, then refresh the list and select it again.", id.path)),
        Some(c) if c.id != *id => Err(format!("The drive at {} is not the one you selected any more (its name, serial number or size changed). Refresh the list and choose again.", id.path)),
        Some(c) => match &c.blocked {
            Some(why) => Err(format!("{} cannot be used: {}.", id.path, why.to_lowercase())),
            None => Ok(c),
        },
    }
}

/// Extra engine flags a candidate needs.
pub fn engine_flags(c: &Candidate) -> Vec<String> {
    if c.drive.safety == Safety::Virtual && !c.test_file {
        vec!["--allow-virtual".into()]
    } else {
        vec![]
    }
}
