//! Which drives a tool may offer. The rules are the engine's own (`nyforge-core`); the terminal adds no looser ones.

use nyforge_core::device::{self, Bus, Drive, Safety};
use nyforge_core::format::human_size;
use nyforge_core::usbformat;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Flash,
    Format,
    Test,
}

#[derive(Clone, PartialEq, Eq)]
pub struct DriveId {
    path: String,
    model: String,
    serial: Option<String>,
    size: u64,
}

#[derive(Clone)]
pub struct Candidate {
    pub id: DriveId,
    pub drive: Drive,
    pub name: String,
    /// Why it cannot be chosen. It is still listed so nobody wonders where it went.
    pub blocked: Option<String>,
    pub warn: Option<String>,
    pub virt: bool,
}

impl Candidate {
    pub fn size_text(&self) -> String {
        human_size(self.drive.size)
    }
    pub fn flags(&self) -> Vec<String> {
        if self.virt {
            vec!["--allow-virtual".into()]
        } else {
            vec![]
        }
    }
}

fn candidate(d: Drive, blocked: Option<String>) -> Candidate {
    let warn = if blocked.is_some() {
        None
    } else if !matches!(d.bus, Bus::Usb | Bus::Sd) {
        Some(format!("Appears as {}, not USB or SD. Check name and size.", d.bus.label()))
    } else if !d.removable {
        Some("Not reported as removable. Check size.".into())
    } else {
        None
    };
    let virt = d.safety == Safety::Virtual;
    Candidate { id: DriveId { path: d.path.clone(), model: d.model.clone(), serial: d.serial.clone(), size: d.size }, name: d.display_name(), drive: d, blocked, warn, virt }
}

fn allow_virtual() -> bool {
    std::env::var_os("NYFORGE_TUI_ALLOW_VIRTUAL").is_some()
}

pub fn scan(purpose: Purpose) -> Result<Vec<Candidate>, String> {
    let all = device::list_drives().map_err(|e| e.message)?;
    let mut out = vec![];
    for d in all.into_iter().filter(|d| d.size > 0) {
        match purpose {
            Purpose::Format => match usbformat::check_drive_allowed(&d) {
                Ok(()) => out.push(candidate(d, None)),
                Err(e) if e.kind == nyforge_core::ErrorKind::ReadOnly => out.push(candidate(d, Some("write-protected".into()))),
                Err(_) => {}
            },
            Purpose::Test => match d.safety {
                Safety::Safe => out.push(candidate(d, None)),
                Safety::Virtual if allow_virtual() => out.push(candidate(d, None)),
                _ => {}
            },
            Purpose::Flash => match d.safety {
                Safety::Safe if d.read_only => out.push(candidate(d, Some("write-protected".into()))),
                Safety::Safe => out.push(candidate(d, None)),
                Safety::Virtual if allow_virtual() => out.push(candidate(d, None)),
                _ => {}
            },
        }
    }
    out.sort_by(|a, b| a.drive.path.cmp(&b.drive.path));
    Ok(out)
}

/// Look again right now and make sure this is still the same drive. Used right before anything destructive.
pub fn revalidate(purpose: Purpose, c: &Candidate) -> Result<(), String> {
    let now = scan(purpose).map_err(|e| format!("Could not check the drive again: {e}"))?;
    match now.into_iter().find(|n| n.id.path == c.id.path) {
        None => Err(format!("{} is no longer connected. Plug it back in and refresh with r.", c.id.path)),
        Some(n) if n.id != c.id => Err(format!("{} is not the drive you selected any more. Refresh with r and choose again.", c.id.path)),
        Some(n) => match n.blocked {
            Some(why) => Err(format!("{} cannot be used: {why}.", c.id.path)),
            None => Ok(()),
        },
    }
}
