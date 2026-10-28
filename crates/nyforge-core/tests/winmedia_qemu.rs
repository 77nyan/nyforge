//! Boots the Windows install stick we build in QEMU, with stand-ins for Microsoft's
//! boot files, to prove the *stick* is bootable: the firmware finds and starts
//! \EFI\BOOT\BOOTX64.EFI (UEFI), and GRUB loads \bootmgr via `ntldr` (BIOS).
//!
//! What this does NOT prove: that real bootmgr/Windows Setup start from it. That
//! needs a genuine Windows ISO, which was not available when this was written.
//!
//!   cargo test -p nyforge-core --test winmedia_qemu -- --ignored --test-threads=1

use nyforge_core::winmedia::{make_windows_media, WindowsOptions};
use nyforge_core::writer::Target;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

fn have(cmd: &str) -> bool {
    Command::new("sh").args(["-c", &format!("command -v {cmd} >/dev/null")]).status().map(|s| s.success()).unwrap_or(false)
}

fn run(cmd: &mut Command) {
    let o = cmd.output().unwrap();
    assert!(o.status.success(), "{:?}: {}{}", cmd, String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

#[test]
#[ignore]
fn windows_stick_starts_on_bios_and_uefi() {
    for t in ["qemu-system-x86_64", "genisoimage", "wimlib-imagex", "nasm", "grub-mkstandalone"] {
        assert!(have(t), "{t} is not installed: this #[ignore]d test only runs on request, so a missing tool is an error");
    }
    let d = std::env::temp_dir().join(format!("nyforge-winqemu-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let t = d.join("tree");
    std::fs::create_dir_all(t.join("sources")).unwrap();
    std::fs::create_dir_all(t.join("EFI/BOOT")).unwrap();
    std::fs::create_dir_all(t.join("boot")).unwrap();
    let payload = d.join("payload");
    std::fs::create_dir_all(&payload).unwrap();
    std::fs::write(payload.join("x.txt"), "x").unwrap();
    for n in ["boot.wim", "install.wim"] {
        run(Command::new("wimlib-imagex").arg("capture").arg(&payload).arg(t.join("sources").join(n)).args(["i", "--compress=none"]));
    }
    std::fs::write(t.join("boot/bcd"), b"BCD").unwrap();

    // Stand-in for bootmgr: 16-bit code that prints a marker on COM1.
    std::fs::write(
        d.join("stub.asm"),
        "bits 16\norg 0\npush cs\npop ds\nmov si, msg\nagain:\nlodsb\ntest al, al\njz done\nmov dx, 0x3f8\nout dx, al\njmp again\ndone:\nhlt\njmp done\nmsg: db 'NTLDR-STUB-OK',13,10,0\n",
    )
    .unwrap();
    run(Command::new("nasm").args(["-f", "bin", "-o"]).arg(t.join("bootmgr")).arg(d.join("stub.asm")));

    // Stand-in for bootmgfw.efi: a GRUB that prints a marker on COM1.
    std::fs::write(d.join("efi.cfg"), "serial --unit=0 --speed=115200\nterminal_output serial\necho UEFI-STUB-OK\nsleep 30\n").unwrap();
    run(Command::new("grub-mkstandalone")
        .args(["-O", "x86_64-efi", "--modules=serial terminal echo normal sleep", "-o"])
        .arg(t.join("EFI/BOOT/bootx64.efi"))
        .arg(format!("boot/grub/grub.cfg={}", d.join("efi.cfg").display())));

    let iso = d.join("win.iso");
    run(Command::new("genisoimage").args(["-udf", "-iso-level", "1", "-V", "WINTEST", "-o"]).arg(&iso).arg(&t));

    let img = d.join("stick.img");
    let opts = WindowsOptions { file_size: Some(1 << 30), ..Default::default() };
    make_windows_media(&iso, &Target::file(&img), &opts, Arc::new(AtomicBool::new(false)), &mut |_| {}).unwrap();

    let script = repo_root().join("tools/qemu-boot.sh");
    for (mode, marker, secs) in [("bios", "NTLDR-STUB-OK", "40"), ("uefi", "UEFI-STUB-OK", "60")] {
        let out = d.join(mode);
        let o = Command::new("bash").arg(&script).arg(&img).arg(mode).arg(secs).arg(&out).output().unwrap();
        assert!(o.status.success());
        let log = std::fs::read(out.join("serial.log")).unwrap_or_default();
        let log = String::from_utf8_lossy(&log);
        assert!(log.contains(marker), "{mode}: marker {marker} not seen on the serial port; log was:\n{log}");
    }
}

/// The NTFS layout: the firmware starts UEFI:NTFS from the small FAT partition, which loads its NTFS driver and starts
/// \EFI\BOOT\BOOTX64.EFI from the NTFS partition (a GRUB stand-in for Microsoft's bootmgfw.efi). Needs root, a loop
/// device, ntfs-3g and mkntfs.
///
///   cargo test -p nyforge-core --test winmedia_qemu -- --ignored --test-threads=1
#[test]
#[ignore]
fn ntfs_windows_stick_starts_through_uefi_ntfs() {
    for t in ["qemu-system-x86_64", "genisoimage", "wimlib-imagex", "grub-mkstandalone", "ntfs-3g", "losetup"] {
        assert!(have(t), "{t} is not installed: this #[ignore]d test only runs on request, so a missing tool is an error");
    }
    let d = std::env::temp_dir().join(format!("nyforge-winntfs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let t = d.join("tree");
    std::fs::create_dir_all(t.join("sources")).unwrap();
    std::fs::create_dir_all(t.join("EFI/BOOT")).unwrap();
    let payload = d.join("payload");
    std::fs::create_dir_all(&payload).unwrap();
    std::fs::write(payload.join("x.txt"), "x").unwrap();
    for n in ["boot.wim", "install.wim"] {
        run(Command::new("wimlib-imagex").arg("capture").arg(&payload).arg(t.join("sources").join(n)).args(["i", "--compress=none"]));
    }
    std::fs::write(t.join("bootmgr"), b"x").unwrap();
    std::fs::write(d.join("efi.cfg"), "serial --unit=0 --speed=115200\nterminal_output serial\necho NTFS-UEFI-STUB-OK\nsleep 30\n").unwrap();
    run(Command::new("grub-mkstandalone")
        .args(["-O", "x86_64-efi", "--modules=serial terminal echo normal sleep", "-o"])
        .arg(t.join("EFI/BOOT/bootx64.efi"))
        .arg(format!("boot/grub/grub.cfg={}", d.join("efi.cfg").display())));
    let iso = d.join("win.iso");
    run(Command::new("genisoimage").args(["-udf", "-iso-level", "1", "-V", "WINNTFS", "-o"]).arg(&iso).arg(&t));

    let img = d.join("stick.img");
    let opts = WindowsOptions { file_size: Some(1 << 30), layout: nyforge_core::winntfs::WinLayout::Ntfs, ..Default::default() };
    make_windows_media(&iso, &Target::file(&img), &opts, Arc::new(AtomicBool::new(false)), &mut |_| {}).unwrap();

    let script = repo_root().join("tools/qemu-boot.sh");
    let out = d.join("uefi");
    let o = Command::new("bash").arg(&script).arg(&img).arg("uefi").arg("90").arg(&out).output().unwrap();
    assert!(o.status.success());
    let log = String::from_utf8_lossy(&std::fs::read(out.join("serial.log")).unwrap_or_default()).into_owned();
    assert!(log.contains("NTFS-UEFI-STUB-OK"), "the stand-in on the NTFS partition did not start; log was:\n{log}");
}
