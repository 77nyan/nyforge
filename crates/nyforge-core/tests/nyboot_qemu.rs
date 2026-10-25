//! End-to-end NyBoot tests: build a drive image, boot it in QEMU (BIOS and UEFI)
//! and read GRUB's output from the serial port.
//!
//! They need qemu-system-x86_64, OVMF, xorriso and a few minutes, so they are
//! `#[ignore]`d by default:   cargo test -p nyforge-core --test nyboot_qemu -- --ignored --test-threads=1
//! (They skip themselves, with a message, when a tool is missing.)

use nyforge_core::image;
use nyforge_core::nyboot::{drive, install::{install, InstallOptions}};
use nyforge_core::writer::Target;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

fn have(cmd: &str) -> bool {
    Command::new("sh").args(["-c", &format!("command -v {cmd} >/dev/null")]).status().map(|s| s.success()).unwrap_or(false)
}

fn need(tools: &[&str]) -> bool {
    for t in tools {
        if !have(t) {
            panic!("{t} is not installed: these tests are #[ignore]d and only run on request, so a missing tool is an error, not a skip");
        }
    }
    if !Path::new("/usr/share/OVMF/OVMF_CODE_4M.fd").exists() && tools.contains(&"qemu-system-x86_64") {
        panic!("OVMF firmware not found at /usr/share/OVMF/OVMF_CODE_4M.fd");
    }
    true
}

fn workdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nyforge-qemu-{}-{}", std::process::id(), name));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Build an ISO9660 image from `(path, bytes)` pairs.
fn make_iso(dir: &Path, name: &str, label: &str, files: &[(&str, &str)]) -> PathBuf {
    let tree = dir.join(format!("{name}.tree"));
    for (p, content) in files {
        let f = tree.join(p);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(&f, content).unwrap();
    }
    let out = dir.join(format!("{name}.iso"));
    let st = Command::new("xorriso")
        .args(["-as", "mkisofs", "-r", "-V", label, "-o"])
        .arg(&out)
        .arg(&tree)
        .output()
        .unwrap();
    assert!(st.status.success(), "xorriso: {}", String::from_utf8_lossy(&st.stderr));
    out
}

const SERIAL_SETUP: &str = "serial --unit=0 --speed=115200\nterminal_input serial console\nterminal_output serial console\nset nb_text=1\n";

fn build_drive(dir: &Path, test_cfg: &str, isos: &[(&Path, &str)]) -> PathBuf {
    let img = dir.join("nyboot.img");
    let opts = InstallOptions {
        file_size: Some(1 << 30),
        extra_esp_files: vec![("grub/test.cfg".into(), format!("{SERIAL_SETUP}{test_cfg}").into_bytes())],
        ..Default::default()
    };
    install(&Target::file(&img), &opts, Arc::new(AtomicBool::new(false)), &mut |_| {}).unwrap();
    let mut od = drive::open(&Target::file(&img), true).unwrap();
    for (iso, folder) in isos {
        drive::add_image(&mut od, image::open_file(iso).unwrap(), folder, Arc::new(AtomicBool::new(false)), &mut |_| {}).unwrap();
    }
    img
}

/// Boot the image; returns the serial log with terminal escapes removed, one line per element.
fn boot(img: &Path, mode: &str, secs: u32, out: &Path) -> Vec<String> {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/qemu-boot.sh");
    let st = Command::new(script).arg(img).arg(mode).arg(secs.to_string()).arg(out).output().unwrap();
    assert!(st.status.success(), "qemu-boot.sh failed: {}", String::from_utf8_lossy(&st.stderr));
    let raw = std::fs::read(out.join("serial.log")).unwrap_or_default();
    let text = String::from_utf8_lossy(&raw).into_owned();
    let mut clean = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Terminal control sequence: drop it, but keep a line break so text does not run together.
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(&n) = chars.peek() {
                    chars.next();
                    if n.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            clean.push('\n');
        } else if c != '\r' {
            clean.push(c);
        }
    }
    // GRUB's serial console wraps at 79 columns (an 80-column screen). Genuine output lines of ours start with
    // "NBX", so a full-width line followed by a non-NBX line is one wrapped line.
    let raw_lines: Vec<&str> = clean.lines().collect();
    let mut out_lines: Vec<String> = vec![];
    let mut i = 0;
    while i < raw_lines.len() {
        let mut l = raw_lines[i].to_string();
        while l.chars().count() == 79 && i + 1 < raw_lines.len() && !raw_lines[i + 1].trim_start().starts_with("NBX") {
            i += 1;
            l.push_str(raw_lines[i]);
        }
        out_lines.push(l.trim().to_string());
        i += 1;
    }
    out_lines.retain(|l| !l.is_empty());
    out_lines
}

#[test]
#[ignore]
fn menu_finds_and_boots_an_iso_in_bios_and_uefi() {
    if !need(&["qemu-system-x86_64", "xorriso"]) {
        return;
    }
    let d = workdir("smoke");
    let iso = make_iso(
        &d,
        "marker",
        "MARKER",
        &[(
            "boot/grub/grub.cfg",
            "serial --unit=0 --speed=115200\nterminal_input serial console\nterminal_output serial console\necho \"NBX-GUEST-GRUB platform=$grub_platform\"\nhalt\n",
        )],
    );
    // Boot the first entry on its own after 1 second.
    let img = build_drive(&d, "set nb_timeout=1\necho \"NBX-MENU platform=$grub_platform\"\n", &[(iso.as_path(), "iso/sub")]);
    for (mode, plat) in [("bios", "pc"), ("uefi", "efi")] {
        let lines = boot(&img, mode, 120, &d.join(mode));
        let all = lines.join("\n");
        assert!(all.contains(&format!("NBX-MENU platform={plat}")), "{mode}: menu never appeared:\n{all}");
        assert!(all.contains(&format!("NBX-GUEST-GRUB platform={plat}")), "{mode}: the ISO's own GRUB never ran:\n{all}");
    }
}

/// `(iso name, label, files)` for one synthetic distro image.
type Spec = (&'static str, &'static str, Vec<(&'static str, &'static str)>);

#[test]
#[ignore]
fn distro_profiles_choose_the_right_commands() {
    if !need(&["qemu-system-x86_64", "xorriso"]) {
        return;
    }
    let d = workdir("profiles");
    let specs: Vec<Spec> = vec![
        ("a-ubuntu", "UBUNTU", vec![("casper/vmlinuz", "k"), ("casper/initrd", "i"), (".disk/info", "x")]),
        ("b-loopbackcfg", "LB", vec![("boot/grub/loopback.cfg", "menuentry x {}"), ("casper/vmlinuz", "k"), ("casper/initrd", "i")]),
        ("c-debian", "DEBLIVE", vec![("live/vmlinuz-6.1.0-18-amd64", "k"), ("live/initrd.img-6.1.0-18-amd64", "i"), ("live/filesystem.squashfs", "s")]),
        (
            "d-arch",
            "ARCH_202401",
            vec![
                ("arch/boot/x86_64/vmlinuz-linux", "k"),
                ("arch/boot/x86_64/initramfs-linux.img", "i"),
                ("arch/boot/intel_ucode.img", "u"),
                ("arch/boot/amd_ucode.img", "u"),
            ],
        ),
        (
            "d2-cachyos",
            "CACHYOS_2510",
            vec![
                ("arch/boot/x86_64/vmlinuz-linux-cachyos", "k"),
                ("arch/boot/x86_64/initramfs-linux-cachyos.img", "i"),
                ("arch/boot/x86_64/vmlinuz-linux-cachyos-lts", "k"),
                ("arch/boot/x86_64/initramfs-linux-cachyos-lts.img", "i"),
                ("arch/boot/intel_ucode.img", "u"),
                ("arch/boot/amd_ucode.img", "u"),
                ("boot/grub/grub.cfg", "menuentry x {}"),
            ],
        ),
        (
            "d3-sysrescue",
            "RESCUE1201",
            vec![("sysresccd/boot/x86_64/vmlinuz", "k"), ("sysresccd/boot/x86_64/sysresccd.img", "i"), ("sysresccd/x86_64/airootfs.sfs", "s")],
        ),
        (
            "d4-archloop",
            "ARCH_LB",
            vec![
                (
                    "boot/grub/loopback.cfg",
                    "search --no-floppy --set=archiso_img_dev --file \"${iso_path}\"\nprobe --set archiso_img_dev_uuid --fs-uuid \"${archiso_img_dev}\"\n",
                ),
                ("arch/boot/x86_64/vmlinuz-linux", "k"),
                ("arch/boot/x86_64/initramfs-linux.img", "i"),
            ],
        ),
        ("e-manjaro", "MANJARO_KDE", vec![("boot/vmlinuz-x86_64", "k"), ("boot/initramfs-x86_64.img", "i"), ("manjaro/x86_64/rootfs.sfs", "r")]),
        ("f-fedora", "Fedora-WS-Live-39", vec![("images/pxeboot/vmlinuz", "k"), ("images/pxeboot/initrd.img", "i")]),
        ("g-windows", "CCCOMA_X64FRE", vec![("sources/boot.wim", "w"), ("bootmgr", "b")]),
        ("g2-generic", "MYLIVE", vec![("boot/vmlinuz-6.1", "k"), ("boot/initrd.img-6.1", "i")]),
        ("h-unknown", "WHAT", vec![("readme.txt", "hello")]),
        ("with space", "SPACEY", vec![("casper/vmlinuz", "k"), ("casper/initrd", "i")]),
    ];
    let isos: Vec<PathBuf> = specs.iter().map(|(n, l, f)| make_iso(&d, n, l, f)).collect();
    // The last one goes into a folder whose name has a space.
    let plan: Vec<(&Path, &str)> = isos
        .iter()
        .enumerate()
        .map(|(i, p)| (p.as_path(), if i == specs.len() - 1 { "iso/My Folder" } else { "iso" }))
        .collect();
    let cfg = "set nb_timeout=-1\nset nb_dryrun=1\n\
               function nb_test_each { nb_boot \"$1\"; }\nset nb_each_iso=nb_test_each\n\
               function nb_autopick_run { echo \"NBX-ALL-DONE\"; halt; }\nset nb_autopick=1\n";
    let img = build_drive(&d, cfg, &plan);
    let lines = boot(&img, "bios", 170, &d.join("bios"));

    // Group per ISO; each command becomes "name arg arg …" (one NBX line + NBX-A argument lines).
    let mut groups: Vec<(String, Vec<String>)> = vec![];
    for l in &lines {
        if let Some(arg) = l.strip_prefix("NBX-A: ") {
            if let Some(cmd) = groups.last_mut().and_then(|g| g.1.last_mut()) {
                cmd.push(' ');
                cmd.push_str(arg);
            }
        } else if let Some(rest) = l.strip_prefix("NBX: iso") {
            groups.push((String::new(), vec![format!("iso{rest}")]));
        } else if let Some(rest) = l.strip_prefix("NBX: ") {
            if let Some(g) = groups.last_mut() {
                g.1.push(rest.to_string());
            }
        }
    }
    // The first arg of "iso" is the ISO path: lift it into the group name.
    for g in groups.iter_mut() {
        let first = g.1[0].clone();
        g.0 = first.trim_start_matches("iso").trim().to_string();
    }
    let dump = format!("{lines:#?}");
    assert!(lines.iter().any(|l| l.contains("NBX-ALL-DONE")), "script did not finish:\n{dump}");
    let find = |name: &str| -> &Vec<String> {
        &groups.iter().find(|(p, _)| p.ends_with(name)).unwrap_or_else(|| panic!("{name} was not processed:\n{dump}")).1
    };
    let has = |g: &Vec<String>, needle: &str| g.iter().any(|l| l.contains(needle));

    let g = find("a-ubuntu.iso");
    assert!(has(g, "profile casper"), "{g:?}");
    assert!(has(g, "linux (loop)/casper/vmlinuz boot=casper iso-scan/filename=/iso/a-ubuntu.iso quiet splash ---"), "{g:?}");
    assert!(has(g, "initrd (loop)/casper/initrd"), "{g:?}");

    let g = find("b-loopbackcfg.iso");
    assert!(has(g, "profile loopbackcfg"), "the distro's own loopback.cfg must win over guessing: {g:?}");
    assert!(has(g, "iso_path=/iso/b-loopbackcfg.iso"), "{g:?}");

    let g = find("c-debian.iso");
    assert!(has(g, "profile debian-live"), "{g:?}");
    assert!(has(g, "linux (loop)/live/vmlinuz-6.1.0-18-amd64 boot=live findiso=/iso/c-debian.iso"), "{g:?}");
    assert!(has(g, "initrd (loop)/live/initrd.img-6.1.0-18-amd64"), "{g:?}");

    let g = find("d-arch.iso");
    assert!(has(g, "profile archiso"), "{g:?}");
    assert!(has(g, "img_loop=/iso/d-arch.iso"), "{g:?}");
    assert!(has(g, "img_dev=/dev/disk/by-uuid/"), "{g:?}");
    assert!(has(g, "initrd (loop)/arch/boot/intel_ucode.img (loop)/arch/boot/amd_ucode.img (loop)/arch/boot/x86_64/initramfs-linux.img"), "{g:?}");

    let g = find("d2-cachyos.iso");
    assert!(has(g, "profile archiso"), "CachyOS names its kernel vmlinuz-linux-cachyos: {g:?}");
    assert!(has(g, "linux (loop)/arch/boot/x86_64/vmlinuz-linux-cachyos archisobasedir=arch img_dev=/dev/disk/by-uuid/"), "{g:?}");
    assert!(has(g, "img_loop=/iso/d2-cachyos.iso cow_spacesize=10G"), "{g:?}");
    assert!(has(g, "initrd (loop)/arch/boot/intel_ucode.img (loop)/arch/boot/amd_ucode.img (loop)/arch/boot/x86_64/initramfs-linux-cachyos.img"), "{g:?}");

    let g = find("d4-archloop.iso");
    // Arch's loopback.cfg would run `search --file` over every disk of the computer; we start the kernel ourselves instead.
    assert!(has(g, "profile archiso"), "{g:?}");
    assert!(!has(g, "configfile"), "{g:?}");
    assert!(has(g, "img_loop=/iso/d4-archloop.iso"), "{g:?}");

    let g = find("d3-sysrescue.iso");
    assert!(has(g, "profile archiso"), "{g:?}");
    assert!(has(g, "linux (loop)/sysresccd/boot/x86_64/vmlinuz archisobasedir=sysresccd"), "{g:?}");
    assert!(has(g, "initrd (loop)/sysresccd/boot/x86_64/sysresccd.img"), "{g:?}");

    let g = find("e-manjaro.iso");
    assert!(has(g, "profile manjaro"), "{g:?}");
    assert!(has(g, "misolabel=MANJARO_KDE"), "{g:?}");

    let g = find("f-fedora.iso");
    assert!(has(g, "profile dracut-live"), "{g:?}");
    assert!(has(g, "root=live:CDLABEL=Fedora-WS-Live-39 rd.live.image iso-scan/filename=/iso/f-fedora.iso"), "{g:?}");

    let g = find("g-windows.iso");
    assert!(has(g, "profile windows"), "{g:?}");

    let g = find("g2-generic.iso");
    assert!(has(g, "profile generic"), "{g:?}");
    assert!(has(g, "linux (loop)/boot/vmlinuz-6.1 iso-scan/filename=/iso/g2-generic.iso findiso=/iso/g2-generic.iso"), "{g:?}");
    assert!(has(g, "initrd (loop)/boot/initrd.img-6.1"), "{g:?}");

    let g = find("h-unknown.iso");
    assert!(g.iter().all(|l| !l.starts_with("linux ")), "an unknown ISO must not get a kernel command line: {g:?}");

    let g = find("with space.iso");
    assert!(has(g, "profile casper"), "files in folders with spaces must be found: {g:?}");
    assert!(has(g, "iso-scan/filename=/iso/My Folder/with space.iso"), "{g:?}");
    let _ = std::fs::remove_dir_all(&d); // only reached when everything above passed; a failing run keeps its files for inspection
}

/// Boots a REAL Linux kernel out of an ISO file on the NyBoot drive.
/// Needs `NYFORGE_TEST_KERNEL_DIR` to point at a folder with `vmlinuz` and `initrd.gz`
/// where the initramfs prints `NBX-REAL-KERNEL-BOOTED` and its /proc/cmdline as
/// `NBX-CMDLINE: …` on the serial console and powers off (see tools/make-test-kernel.sh).
#[test]
#[ignore]
fn a_real_kernel_boots_from_an_iso_in_bios_and_uefi() {
    let kdir = std::env::var("NYFORGE_TEST_KERNEL_DIR").expect("set NYFORGE_TEST_KERNEL_DIR (see tools/make-test-kernel.sh)");
    if !need(&["qemu-system-x86_64", "xorriso"]) {
        return;
    }
    let d = workdir("kernel");
    let tree = d.join("live.tree/casper");
    std::fs::create_dir_all(&tree).unwrap();
    std::fs::copy(Path::new(&kdir).join("vmlinuz"), tree.join("vmlinuz")).unwrap();
    std::fs::copy(Path::new(&kdir).join("initrd.gz"), tree.join("initrd")).unwrap();
    let iso = d.join("live.iso");
    let st = Command::new("xorriso").args(["-as", "mkisofs", "-r", "-V", "LIVE", "-o"]).arg(&iso).arg(d.join("live.tree")).output().unwrap();
    assert!(st.status.success());

    let img = build_drive(&d, "set nb_timeout=1\n", &[(iso.as_path(), "iso")]);
    // A per-ISO override file next to the ISO: send the kernel's console to the serial port.
    let over = b"set nb_params=\"console=ttyS0,115200\"\n";
    let mut od = drive::open(&Target::file(&img), true).unwrap();
    od.fs.add_file("iso/live.iso.nyboot.cfg", &mut &over[..], over.len() as u64, &mut |_| true).unwrap();
    drop(od);

    for mode in ["bios", "uefi"] {
        let lines = boot(&img, mode, 170, &d.join(mode));
        let all = lines.join("\n");
        assert!(all.contains("NBX-REAL-KERNEL-BOOTED"), "{mode}: kernel did not start:\n{all}");
        let cmd = lines.iter().find(|l| l.starts_with("NBX-CMDLINE:")).unwrap_or_else(|| panic!("{mode}: no cmdline\n{all}"));
        assert!(cmd.contains("boot=casper"), "{mode}: {cmd}");
        assert!(cmd.contains("iso-scan/filename=/iso/live.iso"), "{mode}: {cmd}");
        assert!(cmd.contains("console=ttyS0,115200"), "{mode}: the per-ISO override must reach the kernel: {cmd}");
    }
}

/// Copy ISOs onto the data partition with an independent exFAT driver (what a file manager does), then check
/// that NyForge reads the drive and that the menu finds every ISO. Needs root, losetup and exfat-fuse.
#[test]
#[ignore]
fn isos_dropped_in_with_a_file_manager_show_up_in_the_menu() {
    if !need(&["qemu-system-x86_64", "xorriso", "losetup", "mount.exfat-fuse", "fusermount"]) {
        return;
    }
    let d = workdir("dropin");
    let guest = |n: &str| format!("serial --unit=0 --speed=115200\nterminal_input serial console\nterminal_output serial console\necho \"NBX-GUEST-GRUB {n}\"\nhalt\n");
    let a = make_iso(&d, "alpha", "ALPHA", &[("boot/grub/grub.cfg", &guest("alpha"))]);
    let b = make_iso(&d, "beta", "BETA", &[("boot/grub/grub.cfg", &guest("beta"))]);
    let img = build_drive(&d, "set nb_timeout=1\nfunction nb_say {\n  echo \"NBX-ISO $1\"\n}\nset nb_each_iso=nb_say\n", &[]);

    let sh = |cmd: &str| {
        let o = Command::new("sh").arg("-c").arg(cmd).output().unwrap();
        assert!(o.status.success(), "{cmd}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    // Attach just the data partition (found from the MBR), so this works without udev or partition scanning.
    let mbr = { use std::io::Read; let mut v = vec![0u8; 512]; std::fs::File::open(&img).unwrap().read_exact(&mut v).unwrap(); v }; let mbr = mbr;
    let parts = nyforge_core::nyboot::layout::parse_mbr(&mbr[..512]).expect("partition table");
    let data = parts.iter().find(|p| p.0 == 0x07).expect("data partition");
    let lo = sh(&format!("losetup -f --show -o {} --sizelimit {} {}", data.1 * 512, data.2 * 512, img.display()));
    let part = lo.clone();
    let mnt = d.join("mnt");
    std::fs::create_dir_all(&mnt).unwrap();
    let result = std::panic::catch_unwind(|| {
        sh(&format!("mount.exfat-fuse {part} {}", mnt.display()));
        // Fragment the free space first, so the big copy is not contiguous.
        for i in 0..40 {
            sh(&format!("dd if=/dev/zero of={}/pad{i}.bin bs=1k count=96 2>/dev/null", mnt.display()));
        }
        for i in (0..40).step_by(2) {
            std::fs::remove_file(mnt.join(format!("pad{i}.bin"))).unwrap();
        }
        std::fs::copy(&a, mnt.join("Alpha Linux.iso")).unwrap();
        std::fs::create_dir_all(mnt.join("More ISOs/deep")).unwrap();
        std::fs::copy(&b, mnt.join("More ISOs/deep/beta.ISO")).unwrap();
        sh(&format!("sync; fusermount -u {}", mnt.display()));
    });
    sh(&format!("losetup -d {lo}"));
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }

    // NyForge's own reader sees what the file manager wrote.
    let od = drive::open(&Target::file(&img), true).unwrap();
    let top: Vec<String> = od.fs.list("/").unwrap().into_iter().map(|e| e.name).collect();
    assert!(top.iter().any(|n| n == "Alpha Linux.iso"), "NyForge cannot see the dropped-in ISO: {top:?}");
    let deep: Vec<String> = od.fs.list("More ISOs/deep").unwrap().into_iter().map(|e| e.name).collect();
    assert!(deep.iter().any(|n| n == "beta.ISO"), "NyForge cannot see the ISO in the sub-folder: {deep:?}");
    drop(od);
    for (mode, plat) in [("bios", "pc"), ("uefi", "efi")] {
        let all = boot(&img, mode, 120, &d.join(mode)).join("\n");
        assert!(all.contains("Alpha Linux"), "{mode}: top-level ISO not found:\n{all}");
        assert!(all.lines().any(|l| l.trim_start_matches(['*', ' ']) == "beta"), "{mode}: ISO in a sub-folder not found:\n{all}");
        assert!(all.contains("NBX-GUEST-GRUB"), "{mode} ({plat}): the dropped-in ISO did not boot:\n{all}");
    }
    let _ = std::fs::remove_dir_all(&d);
}
