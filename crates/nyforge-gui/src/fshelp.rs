//! Plain-words help for a filesystem that NyFormat greys out: why it cannot be used here, and how to make it work.
//! Pure functions (the operating system is a parameter), so every system's text is tested on every system.

use nyforge_core::usbformat::Fs;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Os {
    Linux,
    Windows,
    MacOs,
}

impl Os {
    pub fn current() -> Os {
        if cfg!(windows) {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::MacOs
        } else {
            Os::Linux
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Os::Linux => "Linux",
            Os::Windows => "Windows",
            Os::MacOs => "macOS",
        }
    }
}

/// Why the choice is greyed out.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reason {
    /// This computer has no program that can make the filesystem (Linux: the package is not installed).
    ToolMissing,
    /// This operating system cannot make the filesystem at all.
    NotOnThisSystem,
    /// The drive is smaller than the filesystem needs.
    TooSmall,
}

pub struct Help {
    pub title: String,
    pub why: String,
    /// Commands to copy, if any.
    pub commands: Vec<String>,
    /// "How it is on each system", this computer's first.
    pub systems: Vec<(Os, String)>,
    pub after: String,
}

fn is_ext(fs: Fs) -> bool {
    fs.is_ext()
}

/// What each system can do for `fs` (one sentence each).
fn per_system(fs: Fs) -> Vec<(Os, String)> {
    let name = fs.name();
    match fs {
        f if is_ext(f) => vec![
            (Os::Linux, format!("{name} can be made once the package e2fsprogs is installed.")),
            (Os::Windows, format!("{name} cannot be made on Windows. Format the drive on a Linux computer, or choose exFAT or NTFS.")),
            (Os::MacOs, format!("{name} cannot be made on macOS. Format the drive on a Linux computer, or choose exFAT.")),
        ],
        Fs::Ntfs => vec![
            (Os::Linux, "NTFS can be made once the package ntfs-3g is installed.".to_string()),
            (Os::Windows, "NTFS works on Windows with nothing to install.".to_string()),
            (Os::MacOs, "NTFS cannot be made on macOS. Format the drive on Windows or Linux, or choose exFAT.".to_string()),
        ],
        _ => vec![],
    }
}

pub fn help(fs: Fs, reason: Reason, drive_bytes: u64, os: Os) -> Help {
    let name = fs.name();
    let mut systems = per_system(fs);
    systems.sort_by_key(|(o, _)| if *o == os { 0 } else { 1 });
    let title = format!("Why can’t I use {name}?");
    match reason {
        Reason::TooSmall => Help {
            title,
            why: format!(
                "This drive holds {} and {name} needs at least {}. Choose another filesystem, or use a bigger drive.",
                nyforge_core::format::human_size(drive_bytes),
                nyforge_core::format::human_size(fs.min_bytes())
            ),
            commands: vec![],
            systems: vec![],
            after: String::new(),
        },
        Reason::ToolMissing => {
            let (pkg_arch, pkg_deb, pkg_fedora, what) = if is_ext(fs) {
                ("e2fsprogs", "e2fsprogs", "e2fsprogs", "mkfs.ext4")
            } else {
                ("ntfs-3g", "ntfs-3g", "ntfs-3g ntfsprogs", "mkfs.ntfs")
            };
            Help {
                title,
                why: format!(
                    "NyForge makes {name} with your system’s own “{what}” program, and it is not installed on this computer. FAT16, FAT32 and exFAT are made by NyForge itself and always work."
                ),
                commands: vec![
                    format!("Arch, Omarchy, Manjaro:  sudo pacman -S --needed {pkg_arch}"),
                    format!("Debian, Ubuntu, Mint:    sudo apt install {pkg_deb}"),
                    format!("Fedora:                  sudo dnf install {pkg_fedora}"),
                ],
                systems,
                after: format!("Then come back here: {name} switches on by itself (select the drive again if it does not)."),
            }
        }
        Reason::NotOnThisSystem => Help {
            title,
            why: format!(
                "{} has no built-in way to make {name}, and NyForge does not carry its own copy of that program. FAT16, FAT32 and exFAT are made by NyForge itself and always work.",
                os.name()
            ),
            commands: vec![],
            systems,
            after: format!("To get a {name} drive, run NyForge on {}.", if fs == Fs::Ntfs { "Windows or Linux" } else { "Linux" }),
        },
    }
}

/// Which `Reason` applies, from what `nyforge-core` says about the filesystem on this computer.
pub fn reason_for(fs: Fs, drive_bytes: u64, os: Os, tool_problem: bool) -> Option<Reason> {
    if tool_problem {
        let can_never = match (os, fs) {
            (Os::Windows, Fs::Ntfs) => false,
            (Os::Windows | Os::MacOs, f) if f.is_ext() => true,
            (Os::MacOs, Fs::Ntfs) => true,
            _ => false,
        };
        return Some(if can_never { Reason::NotOnThisSystem } else { Reason::ToolMissing });
    }
    if drive_bytes > 0 && drive_bytes < fs.min_bytes() {
        return Some(Reason::TooSmall);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_missing_tool_gives_install_commands_for_the_main_families() {
        let r = reason_for(Fs::Ext4, 0, Os::Linux, true).unwrap();
        assert_eq!(r, Reason::ToolMissing);
        let h = help(Fs::Ext4, r, 0, Os::Linux);
        let all = h.commands.join("\n");
        assert!(all.contains("pacman -S --needed e2fsprogs") && all.contains("apt install e2fsprogs") && all.contains("dnf install e2fsprogs"));
        let n = help(Fs::Ntfs, Reason::ToolMissing, 0, Os::Linux);
        assert!(n.commands.join("\n").contains("ntfs-3g"));
        assert_eq!(h.systems.len(), 3, "all three systems are explained");
        assert_eq!(h.systems[0].0, Os::Linux, "this computer's system comes first");
    }

    #[test]
    fn windows_and_macos_say_what_is_impossible_and_why() {
        assert_eq!(reason_for(Fs::Ext3, 0, Os::Windows, true), Some(Reason::NotOnThisSystem));
        assert_eq!(reason_for(Fs::Ext2, 0, Os::MacOs, true), Some(Reason::NotOnThisSystem));
        assert_eq!(reason_for(Fs::Ntfs, 0, Os::MacOs, true), Some(Reason::NotOnThisSystem));
        // NTFS on Windows is never the "impossible" kind (Windows makes it itself).
        assert_eq!(reason_for(Fs::Ntfs, 0, Os::Windows, true), Some(Reason::ToolMissing));
        let h = help(Fs::Ext4, Reason::NotOnThisSystem, 0, Os::Windows);
        assert!(h.why.contains("Windows") && h.after.contains("Linux"));
        assert_eq!(h.systems[0].0, Os::Windows);
        let m = help(Fs::Ntfs, Reason::NotOnThisSystem, 0, Os::MacOs);
        assert!(m.after.contains("Windows or Linux") && m.systems[0].0 == Os::MacOs);
    }

    #[test]
    fn too_small_is_explained_with_both_sizes() {
        let r = reason_for(Fs::Fat32, 10 << 20, Os::Linux, false).unwrap();
        assert_eq!(r, Reason::TooSmall);
        let h = help(Fs::Fat32, r, 10 << 20, Os::Linux);
        assert!(h.why.contains("10") && h.why.contains("FAT32"));
        assert_eq!(reason_for(Fs::Fat32, 8 << 30, Os::Linux, false), None);
    }
}
