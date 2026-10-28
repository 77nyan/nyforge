//! Look inside an ISO and say whether (and how) NyBoot can start it.
//!
//! The detection order mirrors `nb_detect` in `assets/grub/cfg/profiles.cfg`; a
//! test below checks that the two cannot drift apart silently.

use crate::error::Result;
use crate::format::human_size;
use crate::iso::{IsoImage, Kind};
use serde::Serialize;
use std::path::Path;

/// How sure we are that the ISO starts from a multiboot drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    /// The image brings its own loopback menu or follows a widely used convention.
    Expected,
    /// A guess based on how this kind of image usually works; may need a tweak.
    BestEffort,
    /// A Windows installer: needs `nyforge windows`, cannot be loop-booted.
    WindowsInstaller,
    /// Nothing recognisable.
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct WindowsInfo {
    pub boot_wim: bool,
    /// "install.wim" or "install.esd"
    pub install_image: Option<String>,
    pub install_bytes: u64,
    /// Larger than FAT32 allows in one file: needs splitting for a FAT32 stick.
    pub exceeds_fat32: bool,
    pub uefi_x64: bool,
    pub uefi_arm64: bool,
    pub bios_bootmgr: bool,
    /// The ISO brings its own `autounattend.xml` (custom builds such as Tiny10/Tiny11 do): it is kept as it is.
    pub own_unattend: bool,
    /// What kind of Windows disc this looks like, in words ("Windows 10/11 installer", "Windows PE / rescue disc", …).
    pub kind: String,
    /// Build architecture from `sources/idwbinfo.txt` ("amd64", "x86", "arm64"), when the ISO says.
    pub arch: Option<String>,
    /// Things worth knowing before writing it: limits of this exact ISO and of the drive NyForge will make from it.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct IsoReport {
    pub label: String,
    pub container: String,
    pub size: u64,
    pub distro: Option<String>,
    /// Name of the NyBoot start profile ("casper", "archiso", …) or "none".
    pub profile: String,
    pub verdict: Verdict,
    pub uefi_files: bool,
    pub windows: Option<WindowsInfo>,
    pub notes: Vec<String>,
}

const FAT32_MAX_FILE: u64 = 4 * 1024 * 1024 * 1024 - 1;

pub fn analyze_file(path: &Path) -> Result<IsoReport> {
    analyze(&IsoImage::open(path)?, std::fs::metadata(path).map(|m| m.len()).unwrap_or(0))
}

fn first_line(iso: &IsoImage, path: &str) -> Option<String> {
    let e = iso.find(path).ok()??;
    let b = iso.read_all(&e, 64 * 1024).ok()?;
    let s = String::from_utf8_lossy(&b);
    s.lines().map(str::trim).find(|l| !l.is_empty()).map(str::to_string)
}

/// `key=value` from a small text file of the ISO.
fn ini_value(iso: &IsoImage, path: &str, key: &str) -> Option<String> {
    let e = iso.find(path).ok()??;
    let b = iso.read_all(&e, 64 * 1024).ok()?;
    let s = String::from_utf8_lossy(&b);
    s.lines().find_map(|l| {
        let (k, v) = l.split_once('=')?;
        k.trim().eq_ignore_ascii_case(key).then(|| v.trim().to_string())
    })
}

/// Smart recognition of what kind of Windows disc this is, and what that means for making a drive from it. Everything
/// here is read from the ISO's own files; nothing is guessed from the file name.
fn recognise_windows(iso: &IsoImage, mut w: WindowsInfo) -> WindowsInfo {
    let ex = |p: &str| iso.find(p).ok().flatten().is_some();
    w.arch = ini_value(iso, "sources/idwbinfo.txt", "BuildArch").map(|a| a.to_ascii_lowercase());
    let has_swm = ex("sources/install.swm");
    let efi_ms = iso.find("efi/microsoft/boot").ok().flatten().is_some();
    let ia32 = ex("EFI/BOOT/BOOTIA32.EFI");
    let win11_hint = ex("sources/hwreqchk.dll") || (ex("sources/appraiserres.dll") && ex("sources/compatresources.dll"));
    let big_other = iso.walk(1_000_000).map(|f| f.into_iter().any(|(p, sz)| sz > FAT32_MAX_FILE && !p.eq_ignore_ascii_case("sources/install.wim"))).unwrap_or(false);

    let legacy = !w.uefi_x64 && !w.uefi_arm64 && w.bios_bootmgr && efi_ms;
    let arch_word = match w.arch.as_deref() {
        Some("amd64") => " 64-bit",
        Some("x86") => " 32-bit",
        Some("arm64") => " ARM64",
        _ => "",
    };
    w.kind = if w.install_image.is_none() && !has_swm {
        "Windows PE / rescue disc (no install image)".into()
    } else if win11_hint {
        format!("Windows 11{arch_word} installer")
    } else if legacy {
        format!("Windows 7-era{arch_word} installer")
    } else {
        format!("Windows{arch_word} installer")
    };
    if w.install_image.is_none() && !has_swm {
        w.warnings.push("This disc has no install.wim or install.esd, so it is a rescue or deployment disc (Windows PE), not a Windows installer. NyForge copies it and it starts, but there is no Windows to install from it.".into());
    }
    if has_swm {
        w.warnings.push("The install image is already split (install.swm). NyForge copies it as it is.".into());
    }
    if !w.uefi_x64 && !w.uefi_arm64 && w.bios_bootmgr {
        if ia32 {
            w.warnings.push("This disc only has 32-bit UEFI boot files. Most computers that can start it are old; the drive is made as usual.".into());
        } else if efi_ms {
            w.warnings.push("This is an older Windows disc (it has no \\EFI\\BOOT\\BOOTX64.EFI). The drive starts on legacy BIOS computers; UEFI computers need Windows 8 or newer media.".into());
        } else {
            w.warnings.push("This disc has no UEFI boot files, so the drive starts on legacy BIOS computers only.".into());
        }
    }
    if w.uefi_arm64 && !w.uefi_x64 {
        w.warnings.push("This is an ARM64 Windows disc. It starts only on ARM64 computers.".into());
    }
    if let Some(a) = &w.arch {
        if a == "x86" && w.uefi_x64 {
            w.warnings.push("The build says 32-bit (x86) but the disc has 64-bit UEFI files; that is unusual. Tell us if it does not start.".into());
        }
    }
    if big_other {
        w.warnings.push("The disc has a file over 4 GB that is not install.wim. FAT32 cannot hold it, so the drive is made with NTFS and a small UEFI:NTFS boot partition (UEFI computers only).".into());
    }
    if w.exceeds_fat32 && w.install_image.as_deref() == Some("install.esd") {
        w.warnings.push("The install.esd is over 4 GB and cannot be split, so the drive is made with NTFS and a small UEFI:NTFS boot partition (UEFI computers only, Linux and Windows only).".into());
    }
    if w.own_unattend {
        w.warnings.push("The disc has its own autounattend.xml (a custom build such as Tiny10 or Tiny11). NyForge keeps it exactly as it is.".into());
    }
    w
}

fn treeinfo_name(iso: &IsoImage) -> Option<String> {
    let e = iso.find(".treeinfo").ok()??;
    let b = iso.read_all(&e, 64 * 1024).ok()?;
    let s = String::from_utf8_lossy(&b);
    let (mut name, mut ver) = (None, None);
    for l in s.lines() {
        if let Some((k, v)) = l.split_once('=') {
            match k.trim() {
                "name" if name.is_none() => name = Some(v.trim().to_string()),
                "version" if ver.is_none() => ver = Some(v.trim().to_string()),
                _ => {}
            }
        }
    }
    name.map(|n| match ver {
        Some(v) if !n.contains(&v) => format!("{n} {v}"),
        _ => n,
    })
}

/// Arch-family live images (Arch, EndeavourOS, CachyOS, Garuda, SystemRescue …) keep their kernel in
/// `<basedir>/boot/x86_64/` — `arch` for most, something else for a few. The kernel is not always called
/// `vmlinuz-linux` (CachyOS ships `vmlinuz-linux-cachyos`). Returns the base folder and the kernel file name.
pub fn archiso_layout(iso: &IsoImage) -> Option<(String, String)> {
    let top = iso.list("/").ok()?;
    for d in top.iter().filter(|e| e.is_dir) {
        let Ok(files) = iso.list(&format!("{}/boot/x86_64", d.name)) else { continue };
        let mut kernels: Vec<&str> = files.iter().filter(|f| !f.is_dir && f.name.starts_with("vmlinuz") && !f.name.ends_with(".sig")).map(|f| f.name.as_str()).collect();
        kernels.sort();
        let has = |n: &str| files.iter().any(|f| f.name == n);
        // The plain Arch name wins; otherwise the first kernel that has its own initramfs next to it.
        let pick = kernels
            .iter()
            .find(|k| **k == "vmlinuz-linux")
            .or_else(|| kernels.iter().find(|k| has(&format!("initramfs{}.img", &k["vmlinuz".len()..]))))
            // SystemRescue and a few others: one kernel plus a differently named initramfs, with the archiso `x86_64/` folder beside it.
            .or_else(|| kernels.first().filter(|_| iso.exists(&format!("{}/x86_64", d.name)) && files.iter().any(|f| f.name.ends_with(".img") && !f.name.contains("ucode"))));
        if let Some(k) = pick {
            return Some((d.name.clone(), (*k).to_string()));
        }
    }
    None
}

/// Any kernel and initramfs pair in the places live images usually keep them. Used when nothing more specific matched.
pub fn generic_kernel(iso: &IsoImage) -> Option<String> {
    let mut dirs: Vec<String> = vec!["".into(), "boot".into(), "isolinux".into(), "syslinux".into(), "boot/syslinux".into()];
    for d in iso.list("/").ok()?.iter().filter(|e| e.is_dir) {
        for sub in ["", "/boot", "/boot/x86_64", "/x86_64"] {
            dirs.push(format!("{}{sub}", d.name));
        }
    }
    for dir in dirs {
        let Ok(files) = iso.list(&dir) else { continue };
        let lower = |f: &crate::iso::Entry| f.name.to_ascii_lowercase();
        let kernel = files.iter().find(|f| !f.is_dir && (lower(f).starts_with("vmlinuz") || lower(f).starts_with("bzimage")) && !lower(f).ends_with(".sig") && !lower(f).ends_with(".efi"));
        let initrd = files.iter().any(|f| !f.is_dir && (lower(f).starts_with("initrd") || lower(f).starts_with("initramfs")));
        if let (Some(k), true) = (kernel, initrd) {
            return Some(format!("{dir}/{}", k.name).trim_start_matches('/').to_string());
        }
    }
    None
}

pub fn analyze(iso: &IsoImage, size: u64) -> Result<IsoReport> {
    let ex = |p: &str| iso.exists(p);
    let has_dir = |p: &str| iso.find(p).ok().flatten().map(|e| e.is_dir).unwrap_or(false);
    let mut notes: Vec<String> = vec![];

    // Same order as nb_detect.
    let profile = if archiso_layout(iso).is_some() {
        "archiso"
    } else if ex("boot/grub/loopback.cfg") {
        "loopbackcfg"
    } else if has_dir("casper") {
        "casper"
    } else if ex("boot/vmlinuz-x86_64") && has_dir("manjaro") {
        "manjaro"
    } else if has_dir("live") {
        "debian-live"
    } else if ex("images/pxeboot/vmlinuz") {
        "dracut-live"
    } else if ex("sources/boot.wim") || ex("sources/install.wim") || ex("sources/install.esd") {
        "windows"
    } else if ex("boot/grub/grub.cfg") {
        "grubcfg"
    } else if generic_kernel(iso).is_some() {
        "generic"
    } else {
        "none"
    };

    let verdict = match profile {
        "loopbackcfg" | "casper" | "debian-live" => Verdict::Expected,
        "archiso" | "manjaro" | "dracut-live" | "grubcfg" | "generic" => Verdict::BestEffort,
        "windows" => Verdict::WindowsInstaller,
        _ => Verdict::Unknown,
    };

    let distro = first_line(iso, ".disk/info").or_else(|| treeinfo_name(iso)).or_else(|| {
        let (base, kernel) = archiso_layout(iso).filter(|_| profile == "archiso")?;
        Some(if kernel.contains("cachyos") { "CachyOS".to_string() } else if base == "arch" { "Arch-based live image".to_string() } else { format!("Arch-based live image ({base})") })
    });
    let uefi_files = ex("EFI/BOOT/BOOTX64.EFI") || ex("EFI/BOOT/BOOTAA64.EFI") || ex("efi.img") || ex("boot/grub/efi.img");

    let windows = if profile == "windows" {
        let install = ["sources/install.wim", "sources/install.esd"].iter().find_map(|p| iso.find(p).ok().flatten().map(|e| (p.trim_start_matches("sources/").to_string(), e.size)));
        let w = WindowsInfo {
            boot_wim: ex("sources/boot.wim"),
            exceeds_fat32: install.as_ref().map(|i| i.1 > FAT32_MAX_FILE).unwrap_or(false),
            install_bytes: install.as_ref().map(|i| i.1).unwrap_or(0),
            install_image: install.map(|i| i.0),
            uefi_x64: ex("EFI/BOOT/BOOTX64.EFI"),
            uefi_arm64: ex("EFI/BOOT/BOOTAA64.EFI"),
            bios_bootmgr: ex("bootmgr"),
            own_unattend: ex("autounattend.xml"),
            kind: String::new(),
            arch: None,
            warnings: vec![],
        };
        let w = recognise_windows(iso, w);
        notes.extend(w.warnings.iter().cloned());
        if w.exceeds_fat32 {
            notes.push(format!(
                "The install image is {}: bigger than FAT32 allows in one file. `nyforge windows` splits it automatically.",
                human_size(w.install_bytes)
            ));
        }
        Some(w)
    } else {
        None
    };

    match profile {
        "windows" => notes.push("Windows installers cannot start from a file on a multiboot drive. Use `nyforge windows` to make install media.".into()),
        "none" => notes.push("NyBoot does not recognise how this image starts, so it cannot promise it will boot. It may still work if you give it hints in a <name>.iso.nyboot.cfg file next to it.".into()),
        "archiso" | "manjaro" | "dracut-live" => notes.push("The start parameters for this kind of image are based on how it normally works and have not been tried on this exact ISO. If it stops at an emergency shell, tell us which ISO it was.".into()),
        "generic" => notes.push("NyBoot does not know this image's family, so it will try its kernel and initramfs with the loop-boot options most live systems understand. This works for many images; if it stops at an emergency shell, add options in a <name>.iso.nyboot.cfg file next to it.".into()),
        "grubcfg" => notes.push("This image only offers a generic GRUB menu. NyBoot will run it with the ISO as its root, which works for some images and not others.".into()),
        _ => {}
    }
    if profile != "windows" && profile != "none" && !uefi_files {
        notes.push("No UEFI boot files found in the image. NyBoot supplies its own, so this only matters if the image refuses to start under UEFI.".into());
    }

    Ok(IsoReport {
        label: iso.label.clone(),
        container: match iso.kind {
            Kind::Iso9660 => "ISO 9660".into(),
            Kind::Udf => "UDF".into(),
            Kind::Hybrid => "ISO 9660 + UDF".into(),
        },
        size,
        distro,
        profile: profile.to_string(),
        verdict,
        uefi_files,
        windows,
        notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn make_iso(name: &str, files: &[(&str, &[u8])], udf: bool) -> Option<std::path::PathBuf> {
        if Command::new("sh").args(["-c", "command -v xorriso >/dev/null"]).status().ok()?.success() == false {
            return None;
        }
        let d = std::env::temp_dir().join(format!("nyforge-probe-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&d);
        for (p, c) in files {
            let f = d.join("t").join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, c).unwrap();
        }
        let out = d.join("x.iso");
        let mut cmd = Command::new(if udf { "genisoimage" } else { "xorriso" });
        if udf {
            cmd.args(["-udf", "-iso-level", "1", "-V", "WIN", "-o"]);
        } else {
            cmd.args(["-as", "mkisofs", "-r", "-J", "-V", "TESTISO", "-o"]);
        }
        let st = cmd.arg(&out).arg(d.join("t")).output().ok()?;
        st.status.success().then_some(out)
    }

    fn profile_of(name: &str, files: &[(&str, &[u8])]) -> Option<IsoReport> {
        let p = make_iso(name, files, false)?;
        Some(analyze_file(&p).unwrap())
    }

    #[test]
    fn recognises_each_family() {
        let k: &[u8] = b"k";
        let cases: Vec<(&str, Vec<(&str, &[u8])>, &str, Verdict)> = vec![
            ("ubuntu", vec![("boot/grub/loopback.cfg", k), ("casper/vmlinuz", k), (".disk/info", b"Ubuntu 24.04 LTS \"Noble\"\n")], "loopbackcfg", Verdict::Expected),
            ("casper", vec![("casper/vmlinuz", k), ("casper/initrd", k)], "casper", Verdict::Expected),
            ("arch", vec![("arch/boot/x86_64/vmlinuz-linux", k)], "archiso", Verdict::BestEffort),
            ("manjaro", vec![("boot/vmlinuz-x86_64", k), ("manjaro/x", k)], "manjaro", Verdict::BestEffort),
            ("debian", vec![("live/vmlinuz", k), ("live/initrd.img", k)], "debian-live", Verdict::Expected),
            ("fedora", vec![("images/pxeboot/vmlinuz", k), (".treeinfo", b"[general]\nname = Fedora\nversion = 41\n")], "dracut-live", Verdict::BestEffort),
            ("cachyos", vec![("arch/boot/x86_64/vmlinuz-linux-cachyos", k), ("arch/boot/x86_64/initramfs-linux-cachyos.img", k), ("arch/boot/x86_64/vmlinuz-linux-cachyos-lts", k), ("arch/boot/x86_64/initramfs-linux-cachyos-lts.img", k), ("boot/grub/grub.cfg", k)], "archiso", Verdict::BestEffort),
            ("sysrescue", vec![("sysresccd/boot/x86_64/vmlinuz", k), ("sysresccd/boot/x86_64/sysresccd.img", k), ("sysresccd/x86_64/airootfs.sfs", k)], "archiso", Verdict::BestEffort),
            ("grub", vec![("boot/grub/grub.cfg", k)], "grubcfg", Verdict::BestEffort),
            ("isolinux-only", vec![("boot/vmlinuz-6.1", k), ("boot/initrd.img-6.1", k)], "generic", Verdict::BestEffort),
            ("nothing", vec![("hello.txt", k)], "none", Verdict::Unknown),
        ];
        for (name, files, want, verdict) in cases {
            let Some(r) = profile_of(name, &files) else { eprintln!("(xorriso missing: skipped)"); return };
            assert_eq!(r.profile, want, "{name}");
            assert_eq!(r.verdict, verdict, "{name}");
        }
        let r = profile_of("ubuntu2", &[("boot/grub/loopback.cfg", b"x"), (".disk/info", b"Ubuntu 24.04 LTS\n")]).unwrap();
        assert_eq!(r.distro.as_deref(), Some("Ubuntu 24.04 LTS"));
        let r = profile_of("fedora2", &[("images/pxeboot/vmlinuz", b"x"), (".treeinfo", b"[general]\nname = Fedora\nversion = 41\n")]).unwrap();
        assert_eq!(r.distro.as_deref(), Some("Fedora 41"));
    }

    #[test]
    fn windows_installer_is_detected_through_udf() {
        let big = vec![1u8; 10_000];
        let Some(p) = make_iso("win", &[("bootmgr", b"BM"), ("sources/boot.wim", &big), ("sources/install.wim", &big), ("EFI/BOOT/BOOTX64.EFI", b"E")], true) else { return };
        let r = analyze_file(&p).unwrap();
        assert_eq!(r.profile, "windows");
        assert_eq!(r.verdict, Verdict::WindowsInstaller);
        let w = r.windows.unwrap();
        assert!(w.boot_wim && w.uefi_x64 && w.bios_bootmgr && !w.uefi_arm64);
        assert_eq!(w.install_image.as_deref(), Some("install.wim"));
        assert!(!w.exceeds_fat32);
    }

    /// The Rust detection and the GRUB script must use the same order. Pull the
    /// markers out of profiles.cfg's nb_detect and compare with what we test.
    #[test]
    fn detection_order_matches_the_grub_script() {
        let cfg = include_str!("../../../../assets/grub/cfg/profiles.cfg");
        let body = cfg.split("function nb_detect").nth(1).unwrap().split("# ---- profiles").next().unwrap();
        let order: Vec<&str> = body.lines().filter_map(|l| l.split("nb_profile=\"").nth(1)).map(|s| s.split('"').next().unwrap()).collect();
        // debian-live appears twice in the script (with and without .disk/info): same result.
        let mut dedup: Vec<&str> = vec![];
        for o in order {
            if dedup.last() != Some(&o) {
                dedup.push(o);
            }
        }
        assert_eq!(dedup, ["archiso", "loopbackcfg", "casper", "manjaro", "debian-live", "dracut-live", "windows", "grubcfg", "generic"]);
        let src = include_str!("probe.rs");
        let mut pos = 0;
        for name in &dedup {
            let needle = format!("\"{name}\"");
            let at = src[pos..].find(&needle).unwrap_or_else(|| panic!("{name} out of order in probe.rs"));
            pos += at + needle.len();
        }
    }
}
