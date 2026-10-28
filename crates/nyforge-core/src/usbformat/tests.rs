//! End-to-end tests: format image files and ask independent tools (fsck, parted, e2fsprogs, ntfs-3g)
//! whether the result is correct. The tests that need those tools are `#[ignore]`d (run them with
//! `cargo test -p nyforge-core usbformat -- --ignored`); once started, a missing tool is a failure, never a silent skip.

use super::*;
use crate::device::Partition;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nyforge-fmt-{}-{}", std::process::id(), name));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn tool(cmd: &str, args: &[&str]) -> Output {
    Command::new(cmd).args(args).output().unwrap_or_else(|e| panic!("test needs `{cmd}` installed ({e})"))
}

fn ok(cmd: &str, args: &[&str]) -> String {
    let o = tool(cmd, args);
    let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    assert!(o.status.success(), "{cmd} {args:?} failed:\n{text}");
    text
}

fn run(path: &Path, size: u64, opts: FormatOptions) -> FormatReport {
    let mut o = opts;
    o.file_size = Some(size);
    format(&Target::file(path), &o, Arc::new(AtomicBool::new(false)), &mut |_| {}).unwrap()
}

fn one(fs: Fs, size: Option<u64>, label: &str) -> PartSpec {
    PartSpec { fs, size, label: label.into(), cluster: None }
}

fn opts(parts: Vec<PartSpec>, scheme: Scheme) -> FormatOptions {
    FormatOptions { parts, scheme, ..Default::default() }
}

/// Copy one partition out of an image so the single-volume checkers can look at it.
fn extract(img: &Path, start: u64, len: u64, out: &Path) {
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut f = std::fs::File::open(img).unwrap();
    f.seek(SeekFrom::Start(start)).unwrap();
    let mut o = std::fs::File::create(out).unwrap();
    let mut buf = vec![0u8; 1 << 20];
    let mut left = len;
    while left > 0 {
        let n = left.min(buf.len() as u64) as usize;
        f.read_exact(&mut buf[..n]).unwrap();
        o.write_all(&buf[..n]).unwrap();
        left -= n as u64;
    }
}

fn read_at(img: &Path, off: u64, n: usize) -> Vec<u8> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(img).unwrap();
    f.seek(SeekFrom::Start(off)).unwrap();
    let mut b = vec![0u8; n];
    f.read_exact(&mut b).unwrap();
    b
}

fn fsck_vfat(path: &Path) {
    ok("fsck.vfat", &["-n", "-v", path.to_str().unwrap()]);
}

// ------------------------------------------------------------------------------ layouts

#[test]
fn layout_alignment_and_rest() {
    let p = plan_layout(1000 * MIB, Scheme::Mbr, &[one(Fs::Fat32, Some(100 * MIB), "A"), one(Fs::ExFat, None, "B")], 0).unwrap();
    assert_eq!(p[0].start, 2048);
    assert_eq!(p[0].sectors, 100 * MIB / 512);
    assert_eq!(p[1].start, 2048 + 100 * MIB / 512);
    assert_eq!((p[1].start + p[1].sectors) * 512, 1000 * MIB, "the last one takes the rest, MiB aligned");
    assert!(p.iter().all(|x| x.start % 2048 == 0));
    // sizes round up to whole MiB
    let p = plan_layout(1000 * MIB, Scheme::Mbr, &[one(Fs::Fat32, Some(100 * MIB + 1), "A")], 0).unwrap();
    assert_eq!(p[0].sectors * 512, 101 * MIB);
}

#[test]
fn layout_reserve_gpt_and_none() {
    let m = plan_layout(1000 * MIB, Scheme::Mbr, &[one(Fs::Fat32, None, "")], 100 * MIB).unwrap();
    assert_eq!((m[0].start + m[0].sectors) * 512, 900 * MIB);
    let g = plan_layout(1000 * MIB, Scheme::Gpt, &[one(Fs::Fat32, None, "")], 0).unwrap();
    let last = g[0].start + g[0].sectors - 1;
    assert!(last <= 1000 * MIB / 512 - 34 - 1, "must stay out of the backup table");
    let n = plan_layout(1000 * MIB, Scheme::None, &[one(Fs::Fat32, None, "")], 0).unwrap();
    assert_eq!((n[0].start, n[0].sectors), (0, 1000 * MIB / 512));
}

#[test]
fn layout_rejects_nonsense() {
    let d = 1000 * MIB;
    assert!(plan_layout(d, Scheme::Mbr, &[one(Fs::Fat32, None, ""), one(Fs::Fat32, None, "")], 0).is_err(), "two 'rest' partitions");
    assert!(plan_layout(d, Scheme::Mbr, &[one(Fs::Fat32, Some(2000 * MIB), "")], 0).is_err(), "bigger than the drive");
    assert!(plan_layout(d, Scheme::Mbr, &[one(Fs::Fat32, Some(MIB), "")], 0).is_err(), "FAT32 too small");
    assert!(plan_layout(4 * TIB, Scheme::Gpt, &[one(Fs::Fat16, None, "")], 0).is_err(), "FAT16 beyond 2 GiB");
    assert!(plan_layout(d, Scheme::None, &[one(Fs::Fat32, Some(100 * MIB), ""), one(Fs::Fat32, None, "")], 0).is_err());
    let five: Vec<_> = (0..5).map(|_| one(Fs::ExFat, Some(10 * MIB), "")).collect();
    assert!(plan_layout(d, Scheme::Mbr, &five, 0).is_err(), "five MBR partitions");
    assert!(plan_layout(d, Scheme::Gpt, &five, 0).is_ok());
    assert!(plan_layout(d, Scheme::Mbr, &[], 0).is_err());
}

#[test]
fn big_drives_pick_sensible_defaults() {
    assert_eq!(auto_fs(16 * GIB), Fs::Fat32);
    assert_eq!(auto_fs(64 * GIB), Fs::ExFat);
    assert_eq!(auto_scheme(1 * TIB), Scheme::Mbr);
    assert_eq!(auto_scheme(3 * TIB), Scheme::Gpt);
    // MBR cannot see past 2 TiB: the plan stops there instead of failing
    let p = plan_layout(3 * TIB, Scheme::Mbr, &[one(Fs::ExFat, None, "")], 0).unwrap();
    assert!((p[0].start + p[0].sectors) * 512 <= 2 * TIB);
}

#[test]
fn filesystems_have_stable_names_in_json() {
    for (fs, name) in [(Fs::Fat16, "fat16"), (Fs::Fat32, "fat32"), (Fs::ExFat, "exfat"), (Fs::Ntfs, "ntfs"), (Fs::Ext4, "ext4")] {
        assert_eq!(serde_json::to_string(&fs).unwrap(), format!("\"{name}\""));
        assert_eq!(name.parse::<Fs>().unwrap(), fs, "the name in JSON is also what --fs accepts");
    }
    assert!(can_make(Fs::Fat32) && can_make(Fs::ExFat) && can_make(Fs::Fat16));
}

#[test]
fn partition_specs_parse() {
    let s: PartSpec = "fat32:16G:DATA".parse().unwrap();
    assert_eq!(s, PartSpec { fs: Fs::Fat32, size: Some(16 * GIB), label: "DATA".into(), cluster: None });
    let s: PartSpec = "ext4::LINUX".parse().unwrap();
    assert_eq!((s.fs, s.size, s.label.as_str()), (Fs::Ext4, None, "LINUX"));
    let s: PartSpec = "exfat".parse().unwrap();
    assert_eq!((s.fs, s.size, s.label.as_str()), (Fs::ExFat, None, ""));
    let s: PartSpec = "ntfs:1G:X:4K".parse().unwrap();
    assert_eq!(s.cluster, Some(4096));
    assert!("btrfs".parse::<PartSpec>().is_err());
    assert!("fat32:abc".parse::<PartSpec>().is_err());
    assert!("fat32:1G:A:4K:extra".parse::<PartSpec>().is_err());
    assert!("MBR".parse::<Scheme>().is_ok() && "none".parse::<Scheme>().is_ok() && "x".parse::<Scheme>().is_err());
}

// --------------------------------------------------------------------------------- safety

fn drive(path: &str, bus: Bus, safety: Safety) -> Drive {
    Drive {
        path: path.into(),
        vendor: "Test".into(),
        model: "Drive".into(),
        serial: Some("S1".into()),
        size: 8 * GIB,
        sector_size: 512,
        bus,
        removable: matches!(bus, Bus::Usb | Bus::Sd),
        read_only: false,
        safety,
        partitions: vec![],
    }
}

#[test]
fn only_removable_usb_and_sd_pass_the_gate() {
    assert!(check_drive_allowed(&drive("/dev/sdb", Bus::Usb, Safety::Safe)).is_ok());
    assert!(check_drive_allowed(&drive("/dev/mmcblk0", Bus::Sd, Safety::Safe)).is_ok());
    for (bus, safety) in [
        (Bus::Nvme, Safety::System),
        (Bus::Sata, Safety::System),
        (Bus::Usb, Safety::System),
        (Bus::Sata, Safety::Internal),
        (Bus::Nvme, Safety::Internal),
        (Bus::Virtual, Safety::Virtual),
        (Bus::Usb, Safety::Internal),
        (Bus::Sata, Safety::Safe), // mis-classified by the OS as removable: still not USB/SD
        (Bus::Nvme, Safety::Safe),
        (Bus::Mmc, Safety::Safe), // soldered eMMC
        (Bus::Unknown, Safety::Safe),
    ] {
        let e = check_drive_allowed(&drive("/dev/x", bus, safety)).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Unsafe, "{bus:?} {safety:?}");
    }
}

#[test]
fn a_usb_disk_that_is_mounted_as_part_of_an_os_is_refused() {
    for mp in ["/", "/home", "/boot", "/boot/efi", "/usr", "/var"] {
        let mut d = drive("/dev/sdb", Bus::Usb, Safety::Safe);
        d.partitions.push(Partition { path: "/dev/sdb1".into(), number: 1, size: GIB, fstype: Some("ext4".into()), label: None, mountpoints: vec![mp.into()] });
        assert_eq!(check_drive_allowed(&d).unwrap_err().kind, ErrorKind::Unsafe, "{mp}");
    }
    let mut ok_mount = drive("/dev/sdb", Bus::Usb, Safety::Safe);
    ok_mount.partitions.push(Partition { path: "/dev/sdb1".into(), number: 1, size: GIB, fstype: Some("vfat".into()), label: None, mountpoints: vec!["/media/me/USB".into()] });
    assert!(check_drive_allowed(&ok_mount).is_ok(), "an ordinary mounted stick is fine: it gets unmounted");
    let mut ro = drive("/dev/sdb", Bus::Usb, Safety::Safe);
    ro.read_only = true;
    assert_eq!(check_drive_allowed(&ro).unwrap_err().kind, ErrorKind::ReadOnly);
}

#[test]
fn the_list_offers_only_removable_media() {
    let all = vec![
        drive("/dev/nvme0n1", Bus::Nvme, Safety::System),
        drive("/dev/sda", Bus::Sata, Safety::Internal),
        drive("/dev/loop3", Bus::Virtual, Safety::Virtual),
        drive("/dev/sdb", Bus::Usb, Safety::Safe),
        drive("/dev/mmcblk0", Bus::Sd, Safety::Safe),
    ];
    let names: Vec<_> = formattable_drives(all).into_iter().map(|d| d.path).collect();
    assert_eq!(names, vec!["/dev/sdb", "/dev/mmcblk0"]);
}

/// The engine itself refuses: nothing is opened, nothing is written. The paths below do not exist,
/// so if the gate let a call through it would fail with "not found", not with `Unsafe`.
#[test]
fn format_refuses_pc_drives_before_touching_anything() {
    for (bus, safety) in [(Bus::Nvme, Safety::System), (Bus::Sata, Safety::Internal), (Bus::Virtual, Safety::Virtual), (Bus::Sata, Safety::Safe)] {
        let d = drive("/dev/nyforge-test-does-not-exist", bus, safety);
        let t = Target {
            path: PathBuf::from(&d.path),
            display: d.path.clone(),
            kind: TargetKind::BlockDevice,
            capacity: Some(d.size),
            drive: Some(d),
        };
        let o = opts(vec![one(Fs::Fat32, None, "X")], Scheme::Mbr);
        let e = format(&t, &o, Arc::new(AtomicBool::new(false)), &mut |_| {}).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Unsafe, "{bus:?} {safety:?}: {}", e.message);
    }
    // A device node nobody described to us is not formatted either.
    let t = Target { path: PathBuf::from("/dev/nyforge-test-does-not-exist"), display: "x".into(), kind: TargetKind::BlockDevice, capacity: Some(8 * GIB), drive: None };
    let e = format(&t, &opts(vec![one(Fs::Fat32, None, "")], Scheme::Mbr), Arc::new(AtomicBool::new(false)), &mut |_| {}).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Unsafe);
    // A descriptor that does not belong to the path it is attached to.
    let d = drive("/dev/sdb", Bus::Usb, Safety::Safe);
    let t = Target { path: PathBuf::from("/dev/nvme0n1"), display: "x".into(), kind: TargetKind::BlockDevice, capacity: Some(d.size), drive: Some(d) };
    let e = format(&t, &opts(vec![one(Fs::Fat32, None, "")], Scheme::Mbr), Arc::new(AtomicBool::new(false)), &mut |_| {}).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Unsafe);
}

#[test]
fn the_drive_is_looked_up_again_before_writing() {
    // A "USB stick" that the machine does not know at all: the second look finds nothing and refuses.
    let d = drive("/dev/nyforge-test-ghost", Bus::Usb, Safety::Safe);
    let t = Target { path: PathBuf::from(&d.path), display: d.path.clone(), kind: TargetKind::BlockDevice, capacity: Some(d.size), drive: Some(d) };
    let e = format(&t, &opts(vec![one(Fs::Fat32, None, "")], Scheme::Mbr), Arc::new(AtomicBool::new(false)), &mut |_| {}).unwrap_err();
    assert_eq!(e.kind, ErrorKind::DeviceGone, "{}", e.message);
}

// ----------------------------------------------------------------------------- filesystems

#[test]
#[ignore = "needs fsck.vfat, fsck.exfat, e2fsprogs, ntfs-3g, parted, mtools: run with --ignored"]
fn fat32_on_mbr_is_clean_and_knows_where_it_lives() {
    let d = dir("fat32");
    let img = d.join("a.img");
    let r = run(&img, 128 * MIB, opts(vec![one(Fs::Fat32, None, "my usb")], Scheme::Mbr));
    assert_eq!(r.partitions.len(), 1);
    assert_eq!(r.partitions[0].label, "MY USB");
    let mbr = read_at(&img, 0, 512);
    assert_eq!(&mbr[510..], &[0x55, 0xAA]);
    assert_eq!(mbr[446 + 4], 0x0C);
    assert_eq!(u32::from_le_bytes(mbr[446 + 8..446 + 12].try_into().unwrap()), 2048);
    let part = d.join("p.img");
    extract(&img, 2048 * 512, r.partitions[0].size_bytes, &part);
    fsck_vfat(&part);
    let boot = read_at(&part, 0, 512);
    assert_eq!(u32::from_le_bytes(boot[0x1C..0x20].try_into().unwrap()), 2048, "hidden sectors = where the partition starts");
    assert_eq!(&boot[0x52..0x5A], b"FAT32   ");
    assert!(ok("mlabel", &["-s", "-i", part.to_str().unwrap(), "::"]).contains("MY USB"));
    let parted = ok("parted", &["-s", img.to_str().unwrap(), "unit", "MiB", "print"]);
    assert!(parted.contains("msdos") && parted.contains("fat32"), "{parted}");
    std::fs::remove_dir_all(d).ok();
}

#[test]
#[ignore = "needs fsck.vfat, fsck.exfat, e2fsprogs, ntfs-3g, parted, mtools: run with --ignored"]
fn reformatting_the_same_image_again_works() {
    let d = dir("again");
    let img = d.join("a.img");
    run(&img, 64 * MIB, opts(vec![one(Fs::ExFat, None, "ONE")], Scheme::Mbr));
    let r = run(&img, 64 * MIB, opts(vec![one(Fs::Fat32, None, "TWO")], Scheme::Mbr));
    let part = d.join("p.img");
    extract(&img, 2048 * 512, r.partitions[0].size_bytes, &part);
    fsck_vfat(&part);
    std::fs::remove_dir_all(d).ok();
}

#[test]
#[ignore = "needs fsck.vfat, fsck.exfat, e2fsprogs, ntfs-3g, parted, mtools: run with --ignored"]
fn fat16_is_clean() {
    let d = dir("fat16");
    let img = d.join("a.img");
    let r = run(&img, 64 * MIB, opts(vec![one(Fs::Fat16, None, "SMALL")], Scheme::Mbr));
    assert_eq!(read_at(&img, 446 + 4, 1)[0], 0x0E);
    let part = d.join("p.img");
    extract(&img, 2048 * 512, r.partitions[0].size_bytes, &part);
    fsck_vfat(&part);
    assert_eq!(&read_at(&part, 0x36, 8), b"FAT16   ");
    std::fs::remove_dir_all(d).ok();
}

#[test]
#[ignore = "needs fsck.vfat, fsck.exfat, e2fsprogs, ntfs-3g, parted, mtools: run with --ignored"]
fn exfat_is_clean_with_a_unicode_label() {
    let d = dir("exfat");
    let img = d.join("a.img");
    let r = run(&img, 96 * MIB, opts(vec![one(Fs::ExFat, None, "Zażółć")], Scheme::Mbr));
    assert_eq!(read_at(&img, 446 + 4, 1)[0], 0x07);
    let part = d.join("p.img");
    extract(&img, 2048 * 512, r.partitions[0].size_bytes, &part);
    ok("fsck.exfat", &[part.to_str().unwrap()]);
    std::fs::remove_dir_all(d).ok();
}

#[test]
#[ignore = "needs fsck.vfat, fsck.exfat, e2fsprogs, ntfs-3g, parted, mtools: run with --ignored"]
fn cluster_size_is_honoured() {
    let d = dir("cluster");
    let img = d.join("a.img");
    let mut s = one(Fs::Fat32, None, "C");
    s.cluster = Some(1024);
    let r = run(&img, 128 * MIB, opts(vec![s], Scheme::Mbr));
    let part = d.join("p.img");
    extract(&img, 2048 * 512, r.partitions[0].size_bytes, &part);
    let b = read_at(&part, 0, 512);
    assert_eq!(u16::from_le_bytes([b[11], b[12]]) as u32 * b[13] as u32, 1024);
    assert_eq!(&b[0x52..0x5A], b"FAT32   ");
    fsck_vfat(&part);
    // a cluster size that cannot make a FAT32 of this size is refused before anything is erased
    let img2 = d.join("b.img");
    std::fs::write(&img2, b"keep").unwrap();
    let mut bad = one(Fs::Fat32, None, "");
    bad.cluster = Some(8192);
    let mut o = opts(vec![bad], Scheme::Mbr);
    o.file_size = Some(128 * MIB);
    let e = format(&Target::file(&img2), &o, Arc::new(AtomicBool::new(false)), &mut |_| {}).unwrap_err();
    assert_eq!(e.kind, ErrorKind::InvalidInput);
    assert!(e.message.contains("clusters"), "{}", e.message);
    assert_eq!(std::fs::read(&img2).unwrap(), b"keep");
    std::fs::remove_dir_all(d).ok();
}

#[test]
#[ignore = "needs fsck.vfat, fsck.exfat, e2fsprogs, ntfs-3g, parted, mtools: run with --ignored"]
fn no_partition_table_means_the_filesystem_starts_at_sector_zero() {
    let d = dir("none");
    let img = d.join("a.img");
    run(&img, 128 * MIB, opts(vec![one(Fs::Fat32, None, "CAM")], Scheme::None));
    let b = read_at(&img, 0, 512);
    assert_eq!(&b[0x52..0x5A], b"FAT32   ");
    assert_eq!(u32::from_le_bytes(b[0x1C..0x20].try_into().unwrap()), 0);
    fsck_vfat(&img);
    std::fs::remove_dir_all(d).ok();
}

#[test]
#[ignore = "needs fsck.vfat, fsck.exfat, e2fsprogs, ntfs-3g, parted, mtools: run with --ignored"]
fn gpt_with_two_partitions_has_valid_primary_and_backup_tables() {
    let d = dir("gpt");
    let img = d.join("a.img");
    let size = 256 * MIB;
    let r = run(&img, size, opts(vec![one(Fs::Fat32, Some(64 * MIB), "DATA"), one(Fs::ExFat, None, "BIG")], Scheme::Gpt));
    assert_eq!(r.partitions.len(), 2);
    let sectors = size / 512;
    // our own parser, both copies
    let prim = tables::parse_gpt_header_and_entries(&read_at(&img, 512, 512), &read_at(&img, 1024, 16384)).expect("primary GPT");
    let back = tables::parse_gpt_header_and_entries(&read_at(&img, (sectors - 1) * 512, 512), &read_at(&img, (sectors - 33) * 512, 16384)).expect("backup GPT");
    assert_eq!(prim, back);
    assert_eq!(prim.len(), 2);
    assert_eq!(prim[0].3, "DATA");
    assert_eq!(read_at(&img, 446 + 4, 1)[0], 0xEE, "protective MBR");
    // an independent implementation agrees
    let out = tool("parted", &["-s", img.to_str().unwrap(), "unit", "MiB", "print"]);
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "{text}");
    assert!(text.contains("Partition Table: gpt"), "{text}");
    assert!(!text.contains("Warning") && !text.contains("Error"), "parted complains about the table:\n{text}");
    assert!(text.contains("DATA") && text.contains("BIG"), "{text}");
    // and both filesystems are intact
    let p1 = d.join("p1.img");
    let p2 = d.join("p2.img");
    extract(&img, r.partitions[0].start_bytes, r.partitions[0].size_bytes, &p1);
    extract(&img, r.partitions[1].start_bytes, r.partitions[1].size_bytes, &p2);
    fsck_vfat(&p1);
    ok("fsck.exfat", &[p2.to_str().unwrap()]);
    std::fs::remove_dir_all(d).ok();
}

#[test]
#[ignore = "needs fsck.vfat, fsck.exfat, e2fsprogs, ntfs-3g, parted, mtools: run with --ignored"]
fn mbr_with_fat32_and_ext4_side_by_side() {
    let d = dir("mixed");
    let img = d.join("a.img");
    let mut o = opts(vec![one(Fs::Fat32, Some(64 * MIB), "WIN"), one(Fs::Ext4, None, "LINUX")], Scheme::Mbr);
    o.owner = Some((1234, 5678));
    let r = run(&img, 160 * MIB, o);
    assert!(r.notes.iter().any(|n| n.contains("1234:5678")), "{:?}", r.notes);
    assert_eq!(read_at(&img, 446 + 4, 1)[0], 0x0C);
    assert_eq!(read_at(&img, 462 + 4, 1)[0], 0x83);
    let p1 = d.join("p1.img");
    let p2 = d.join("p2.img");
    extract(&img, r.partitions[0].start_bytes, r.partitions[0].size_bytes, &p1);
    extract(&img, r.partitions[1].start_bytes, r.partitions[1].size_bytes, &p2);
    fsck_vfat(&p1);
    ok("e2fsck", &["-fn", p2.to_str().unwrap()]);
    let info = ok("dumpe2fs", &["-h", p2.to_str().unwrap()]);
    assert!(info.contains("Filesystem volume name:   LINUX"), "{info}");
    assert!(info.contains("Reserved block count:     0"), "no 5% reserve on a data stick:\n{info}");
    let root = ok("debugfs", &["-R", "stat /", p2.to_str().unwrap()]);
    assert!(root.contains("User:  1234") && root.contains("Group:  5678"), "root folder owner:\n{root}");
    std::fs::remove_dir_all(d).ok();
}

#[test]
#[ignore = "needs fsck.vfat, fsck.exfat, e2fsprogs, ntfs-3g, parted, mtools: run with --ignored"]
fn ntfs_is_made_by_the_system_tool_and_checks_out() {
    let d = dir("ntfs");
    let img = d.join("a.img");
    let r = run(&img, 128 * MIB, opts(vec![one(Fs::Ntfs, None, "NTFSVOL")], Scheme::Mbr));
    assert_eq!(read_at(&img, 446 + 4, 1)[0], 0x07);
    let part = d.join("p.img");
    extract(&img, r.partitions[0].start_bytes, r.partitions[0].size_bytes, &part);
    ok("ntfsfix", &["-n", part.to_str().unwrap()]);
    let b = read_at(&part, 0, 512);
    assert_eq!(&b[3..11], b"NTFS    ");
    assert_eq!(u32::from_le_bytes(b[0x1C..0x20].try_into().unwrap()), 2048, "NTFS knows where its partition starts");
    std::fs::remove_dir_all(d).ok();
}

#[test]
#[ignore = "needs fsck.vfat, fsck.exfat, e2fsprogs, ntfs-3g, parted, mtools: run with --ignored"]
fn old_filesystem_signatures_do_not_survive_inside_a_new_partition() {
    let d = dir("sig");
    let img = d.join("a.img");
    run(&img, 128 * MIB, opts(vec![one(Fs::Ext4, None, "OLD")], Scheme::Mbr));
    let r = run(&img, 128 * MIB, opts(vec![one(Fs::Fat32, None, "NEW")], Scheme::Mbr));
    let part = d.join("p.img");
    extract(&img, r.partitions[0].start_bytes, r.partitions[0].size_bytes, &part);
    let out = tool("blkid", &["-p", part.to_str().unwrap()]);
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(text.contains("vfat") && !text.contains("ext4") && !text.contains("ambiguous"), "{text} {}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(d).ok();
}

#[test]
fn reserve_leaves_the_end_of_the_drive_alone() {
    let d = dir("reserve");
    let img = d.join("a.img");
    let mut o = opts(vec![one(Fs::Fat32, None, "R")], Scheme::Mbr);
    o.reserve_bytes = 32 * MIB;
    let r = run(&img, 128 * MIB, o);
    assert_eq!(r.partitions[0].start_bytes + r.partitions[0].size_bytes, 96 * MIB);
    std::fs::remove_dir_all(d).ok();
}

// ------------------------------------------------------------------- capacity test, wiping

#[test]
#[ignore = "needs fsck.vfat, fsck.exfat, e2fsprogs, ntfs-3g, parted, mtools: run with --ignored"]
fn capacity_test_on_a_healthy_image_passes_then_formats() {
    let d = dir("cap");
    let img = d.join("a.img");
    let mut o = opts(vec![one(Fs::Fat32, None, "OK")], Scheme::Mbr);
    o.capacity_test = CapacityTest::Full;
    let r = run(&img, 64 * MIB, o);
    let c = r.capacity.unwrap();
    assert!(c.genuine && c.mode == CapacityTest::Full);
    let part = d.join("p.img");
    extract(&img, r.partitions[0].start_bytes, r.partitions[0].size_bytes, &part);
    fsck_vfat(&part); // the test's random data is gone
    std::fs::remove_dir_all(d).ok();
}

#[test]
fn fit_real_capacity_needs_the_full_test_and_mbr() {
    let d = dir("fit");
    let img = d.join("a.img");
    let mut o = opts(vec![one(Fs::Fat32, None, "")], Scheme::Mbr);
    o.fit_real_capacity = true;
    o.file_size = Some(64 * MIB);
    let e = format(&Target::file(&img), &o, Arc::new(AtomicBool::new(false)), &mut |_| {}).unwrap_err();
    assert_eq!(e.kind, ErrorKind::InvalidInput);
    o.capacity_test = CapacityTest::Full;
    o.scheme = Scheme::Gpt;
    let e = format(&Target::file(&img), &o, Arc::new(AtomicBool::new(false)), &mut |_| {}).unwrap_err();
    assert_eq!(e.kind, ErrorKind::InvalidInput);
    assert!(!img.exists(), "bad requests are refused before the image is even created");
    std::fs::remove_dir_all(d).ok();
}

#[test]
fn bad_requests_fail_before_anything_is_erased() {
    let d = dir("early");
    let img = d.join("a.img");
    std::fs::write(&img, b"precious").unwrap();
    let mut o = opts(vec![one(Fs::Fat32, None, "BAD*LABEL")], Scheme::Mbr);
    o.file_size = Some(64 * MIB);
    assert_eq!(format(&Target::file(&img), &o, Arc::new(AtomicBool::new(false)), &mut |_| {}).unwrap_err().kind, ErrorKind::InvalidInput);
    o.parts = vec![one(Fs::Fat32, Some(10 * GIB), "OK")];
    assert_eq!(format(&Target::file(&img), &o, Arc::new(AtomicBool::new(false)), &mut |_| {}).unwrap_err().kind, ErrorKind::InvalidInput);
    o.parts = vec![one(Fs::Fat32, None, "OK")];
    o.file_size = None;
    assert_eq!(format(&Target::file(&img), &o, Arc::new(AtomicBool::new(false)), &mut |_| {}).unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(std::fs::read(&img).unwrap(), b"precious");
    std::fs::remove_dir_all(d).ok();
}

#[test]
fn cancelling_says_the_drive_may_be_half_erased() {
    let d = dir("cancel");
    let img = d.join("a.img");
    let mut o = opts(vec![one(Fs::Fat32, None, "")], Scheme::Mbr);
    o.file_size = Some(64 * MIB);
    o.capacity_test = CapacityTest::Full;
    let e = format(&Target::file(&img), &o, Arc::new(AtomicBool::new(true)), &mut |_| {}).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Cancelled);
    assert!(e.hint.unwrap_or_default().contains("partly erased"));
    std::fs::remove_dir_all(d).ok();
}

#[test]
fn progress_events_have_the_usual_shape() {
    let d = dir("events");
    let img = d.join("a.img");
    let mut o = opts(vec![one(Fs::ExFat, None, "")], Scheme::Mbr);
    o.file_size = Some(64 * MIB);
    o.capacity_test = CapacityTest::Quick;
    let mut ev = vec![];
    format(&Target::file(&img), &o, Arc::new(AtomicBool::new(false)), &mut |e| ev.push(e)).unwrap();
    assert!(matches!(ev.first().unwrap(), Event::Start { .. }));
    assert!(matches!(ev.last().unwrap(), Event::Finished { ok: true, .. }));
    assert!(ev.iter().any(|e| matches!(e, Event::Progress(p) if p.fraction > 0.0)));
    std::fs::remove_dir_all(d).ok();
}

// ------------------------------------------------------------- a real block device (loop)

fn loop_target(dev: &str, size: u64) -> Target {
    // The real gate (correctly) says a loop device is not removable media, so the description is made by hand.
    let mut dr = drive(dev, Bus::Usb, Safety::Safe);
    dr.size = size;
    Target { path: PathBuf::from(dev), display: dev.to_string(), kind: TargetKind::BlockDevice, capacity: Some(size), drive: Some(dr) }
}

/// Needs root and `losetup`. Run with `-- --ignored`. A loop device stands in for a USB stick and exercises
/// the parts image files cannot: exclusive uncached (O_DIRECT) opens, BLKZEROOUT, the read-back capacity
/// test through the real block layer, and the system `mkfs` tools writing to a real device node.
///
/// Not covered: the kernel creating `/dev/sdb1`-style partition nodes after the table is rewritten. The loop
/// driver of the machine this was developed on has partitions disabled (`max_part=0`), so ext4/NTFS
/// *inside a partition* of a real drive was never run; here they are run on the whole device instead.
#[test]
#[ignore]
fn a_real_block_device_end_to_end() {
    let d = dir("loop");
    let img = d.join("disk.img");
    let size = 256 * MIB;
    let f = std::fs::File::create(&img).unwrap();
    f.set_len(size).unwrap();
    drop(f);
    let dev = ok("losetup", &["-f", "--show", img.to_str().unwrap()]).trim().to_string();
    let result = std::panic::catch_unwind(|| {
        // A: GPT + FAT32 + exFAT, uncached full capacity test, zero wipe.
        let t = loop_target(&dev, size);
        let mut o = opts(vec![one(Fs::Fat32, Some(64 * MIB), "STICK"), one(Fs::ExFat, None, "BIG")], Scheme::Gpt);
        o.capacity_test = CapacityTest::Full;
        o.wipe = Wipe::Zero;
        let mut saw_progress = false;
        let r = format_unchecked_lookup(&t, &o, Arc::new(AtomicBool::new(false)), &mut |e| saw_progress |= matches!(e, Event::Progress(_))).unwrap();
        assert!(saw_progress);
        assert!(r.capacity.unwrap().genuine);
        let p1 = d.join("p1.img");
        let p2 = d.join("p2.img");
        extract(&img, r.partitions[0].start_bytes, r.partitions[0].size_bytes, &p1);
        extract(&img, r.partitions[1].start_bytes, r.partitions[1].size_bytes, &p2);
        fsck_vfat(&p1);
        ok("fsck.exfat", &[p2.to_str().unwrap()]);
        let parted = tool("parted", &["-s", &dev, "print"]);
        let text = format!("{}{}", String::from_utf8_lossy(&parted.stdout), String::from_utf8_lossy(&parted.stderr));
        assert!(text.contains("gpt") && !text.contains("Warning"), "{text}");
        // the zero wipe reached the gap that no partition owns (everything but the backup table at the very end)
        let gap = read_at(&img, 255 * MIB, (MIB - 20 * 1024) as usize);
        assert!(gap.iter().all(|b| *b == 0), "unpartitioned space was not zeroed");

        // B: random wipe (checked with a filesystem that does not TRIM the device afterwards)...
        let mut o = opts(vec![one(Fs::Fat32, None, "RND")], Scheme::None);
        o.wipe = Wipe::Random;
        format_unchecked_lookup(&loop_target(&dev, size), &o, Arc::new(AtomicBool::new(false)), &mut |_| {}).unwrap();
        let deep = read_at(&img, 200 * MIB, 65536);
        let distinct = deep.iter().collect::<std::collections::HashSet<_>>().len();
        assert!(distinct > 200, "random wipe left only {distinct} distinct byte values");
        // ...then ext4 on the whole device node (no partition table).
        let mut o = opts(vec![one(Fs::Ext4, None, "WHOLE")], Scheme::None);
        o.owner = Some((1000, 1000));
        format_unchecked_lookup(&loop_target(&dev, size), &o, Arc::new(AtomicBool::new(false)), &mut |_| {}).unwrap();
        ok("e2fsck", &["-fn", &dev]);
        let blk = ok("blkid", &["-p", &dev]);
        assert!(blk.contains("ext4") && blk.contains("WHOLE"), "{blk}");

        // C: NTFS on the whole device node, quick wipe.
        let o = opts(vec![one(Fs::Ntfs, None, "NTFSDEV")], Scheme::None);
        format_unchecked_lookup(&loop_target(&dev, size), &o, Arc::new(AtomicBool::new(false)), &mut |_| {}).unwrap();
        ok("ntfsfix", &["-n", &dev]);
        assert!(ok("blkid", &["-p", &dev]).contains("ntfs"));
    });
    let _ = Command::new("losetup").args(["-d", &dev]).output();
    std::fs::remove_dir_all(d).ok();
    result.unwrap();
}

// ------------------------------------------------------------------------------ repair

#[test]
fn repair_says_what_was_wrong_and_clears_what_a_quick_format_would_leave() {
    use std::io::{Seek, SeekFrom, Write};
    let d = dir("repair");
    let img = d.join("stuck.img");
    let size = 200 * MIB;
    {
        let mut f = std::fs::File::create(&img).unwrap();
        f.set_len(size).unwrap();
        // a flashed ISO's leftovers: the CD-ROM marker, and an old MBR with one 30 MiB partition at 20 MiB
        f.seek(SeekFrom::Start(32769)).unwrap();
        f.write_all(b"CD001").unwrap();
        let mut mbr = [0u8; 512];
        mbr[446 + 4] = 0x0C;
        mbr[446 + 8..446 + 12].copy_from_slice(&((20 * MIB / 512) as u32).to_le_bytes());
        mbr[446 + 12..446 + 16].copy_from_slice(&((30 * MIB / 512) as u32).to_le_bytes());
        mbr[510] = 0x55;
        mbr[511] = 0xAA;
        f.seek(SeekFrom::Start(0)).unwrap();
        f.write_all(&mbr).unwrap();
        // stale labels inside the old partition (start and end), which a quick format leaves alone
        f.seek(SeekFrom::Start(20 * MIB + 100)).unwrap();
        f.write_all(b"OLD-FS-LABEL").unwrap();
        f.seek(SeekFrom::Start(50 * MIB - 100)).unwrap();
        f.write_all(b"OLD-FS-TAIL").unwrap();
    }
    let target = Target::file(&img);
    let diag = repair::diagnose(&target, size).unwrap();
    let all = diag.findings.join("\n");
    assert!(all.contains("ISO 9660"), "{all}");
    assert!(all.contains("MBR partition table with 1 partition"), "{all}");
    assert!(all.contains("not in any partition"), "{all}");
    assert_eq!(diag.old_parts, vec![(20 * MIB, 30 * MIB)]);

    let mut o = opts(vec![one(Fs::Fat32, None, "FIXED")], Scheme::Mbr);
    o.repair = true;
    let rep = run(&img, size, o);
    assert!(rep.notes.iter().any(|n| n.starts_with("Found: ")), "{:?}", rep.notes);
    assert!(rep.notes.iter().any(|n| n.contains("cleared")), "{:?}", rep.notes);
    assert_eq!(read_at(&img, 32769, 5), vec![0u8; 5], "the ISO marker is gone");
    assert_eq!(read_at(&img, 20 * MIB + 100, 12), vec![0u8; 12], "the old partition's start is clean");
    assert_eq!(read_at(&img, 50 * MIB - 100, 11), vec![0u8; 11], "the old partition's end is clean");
    assert_eq!(rep.partitions.len(), 1);
    assert!(rep.usable_bytes >= 190 * MIB);
    let _ = std::fs::remove_dir_all(d);
}

// ------------------------------------------------------------------------------ advanced options

#[test]
fn alignment_moves_every_partition_to_the_erase_block_grid() {
    let specs = [one(Fs::Fat32, Some(100 * MIB), "A"), one(Fs::ExFat, None, "B")];
    let p = plan_layout_aligned(1000 * MIB, Scheme::Mbr, &specs, 0, 4 * MIB).unwrap();
    assert_eq!(p[0].start, 8192, "the first partition starts at 4 MiB");
    assert_eq!(p[0].sectors, 100 * MIB / 512);
    assert!(p.iter().all(|x| x.start % 8192 == 0 && x.sectors % 8192 == 0), "{p:?}");
    let q = plan_layout_aligned(1000 * MIB, Scheme::Mbr, &[one(Fs::Fat32, Some(34 * MIB), "A"), one(Fs::ExFat, None, "B")], 0, 8 * MIB).unwrap();
    assert_eq!(q[0].sectors * 512, 40 * MIB, "a size is rounded up to the grid");
    assert!(plan_layout_aligned(1000 * MIB, Scheme::Mbr, &specs, 0, 3 * MIB).is_err());
    assert!(plan_layout_aligned(1000 * MIB, Scheme::Mbr, &specs, 0, 512).is_err());
    let (a, b) = (plan_layout(1000 * MIB, Scheme::Mbr, &specs, 0).unwrap(), plan_layout_aligned(1000 * MIB, Scheme::Mbr, &specs, 0, MIB).unwrap());
    assert!(a.iter().zip(&b).all(|(x, y)| x.start == y.start && x.sectors == y.sectors), "1 MiB is the default grid");
}

#[test]
fn a_formatted_image_honours_alignment_and_the_bootable_flag() {
    let d = dir("align-boot");
    let img = d.join("a.img");
    let mut o = opts(vec![one(Fs::Fat32, None, "ALIGNED")], Scheme::Mbr);
    o.align_bytes = 4 * MIB;
    o.bootable = true;
    let rep = run(&img, 200 * MIB, o);
    assert_eq!(rep.partitions[0].start_bytes, 4 * MIB);
    let mbr = read_at(&img, 0, 512);
    assert_eq!(mbr[446], 0x80, "active flag");
    assert!(rep.notes.iter().any(|n| n.starts_with("Checked:")), "{:?}", rep.notes);
    // not bootable unless asked
    let img2 = d.join("b.img");
    run(&img2, 200 * MIB, opts(vec![one(Fs::Fat32, None, "PLAIN")], Scheme::Mbr));
    assert_eq!(read_at(&img2, 446, 1)[0], 0x00);
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn the_read_back_check_notices_a_missing_filesystem() {
    use std::io::{Seek, SeekFrom, Write};
    let d = dir("readback");
    let img = d.join("a.img");
    let rep = run(&img, 100 * MIB, opts(vec![one(Fs::Fat32, None, "OK")], Scheme::Mbr));
    let mut t = Target::file(&img);
    t.capacity = Some(100 * MIB);
    let plan = plan_layout(100 * MIB, Scheme::Mbr, &[one(Fs::Fat32, None, "OK")], 0).unwrap();
    assert!(check_result(&t, 100 * MIB, Scheme::Mbr, &plan).unwrap());
    // wreck the FAT32 boot sector's signature
    let at = rep.partitions[0].start_bytes + 0x52;
    let mut f = std::fs::OpenOptions::new().write(true).open(&img).unwrap();
    f.seek(SeekFrom::Start(at)).unwrap();
    f.write_all(b"XXXXXXXX").unwrap();
    drop(f);
    let e = check_result(&t, 100 * MIB, Scheme::Mbr, &plan).unwrap_err();
    assert_eq!(e.kind, ErrorKind::VerifyFailed, "{}", e.message);
    // a table that does not match the plan
    let wrong = plan_layout(100 * MIB, Scheme::Mbr, &[one(Fs::Fat32, Some(40 * MIB), "OK")], 0).unwrap();
    assert!(check_result(&t, 100 * MIB, Scheme::Mbr, &wrong).is_err());
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn discard_is_a_wipe_mode_that_never_fails_on_an_image_file() {
    assert_eq!("discard".parse::<Wipe>().unwrap(), Wipe::Discard);
    assert_eq!("trim".parse::<Wipe>().unwrap(), Wipe::Discard);
    let d = dir("discard");
    let img = d.join("a.img");
    let mut o = opts(vec![one(Fs::Fat32, None, "TRIM")], Scheme::Mbr);
    o.wipe = Wipe::Discard;
    let rep = run(&img, 100 * MIB, o);
    assert!(rep.notes.iter().any(|n| n.contains("TRIM") || n.contains("Discard")), "{:?}", rep.notes);
    let _ = std::fs::remove_dir_all(d);
}

// ------------------------------------------------------------------ wipe passes and wipe check (no external tools)

mod wipe {
    use super::*;
    use std::io::Write;

    fn region(name: &str, size: usize, fill: u8) -> (PathBuf, FileIo) {
        let p = dir(name).join("disk.img");
        std::fs::File::create(&p).unwrap().write_all(&vec![fill; size]).unwrap();
        let f = std::fs::OpenOptions::new().read(true).write(true).open(&p).unwrap();
        (p, FileIo::new(f, 0, size as u64, 1))
    }

    fn reporter<'a>(sink: &'a mut dyn FnMut(Event)) -> Reporter<'a> {
        Reporter::new(sink, "test".into())
    }

    #[test]
    fn several_zero_passes_leave_only_zeros_and_are_checked() {
        let (p, io) = region("zero3", 9 << 20, 0xAB);
        let mut logs = vec![];
        let mut sink = |e: Event| {
            if let Event::Log { message } = e {
                logs.push(message)
            }
        };
        let never = AtomicBool::new(false);
        wipe_all(&io, Wipe::Zero, 3, true, &never, &mut reporter(&mut sink)).unwrap();
        assert!(std::fs::read(&p).unwrap().iter().all(|&b| b == 0));
        assert!(logs.iter().any(|l| l.contains("Pass 3 of 3")), "{logs:?}");
        assert!(logs.iter().any(|l| l.contains("Reading the wipe back")));
    }

    #[test]
    fn random_wipe_verifies_with_the_last_passes_seed_and_nothing_else() {
        let (p, io) = region("rand", 5 << 20, 0);
        let mut sink = |_: Event| {};
        let never = AtomicBool::new(false);
        let seed = wipe_all(&io, Wipe::Random, 2, true, &never, &mut reporter(&mut sink)).unwrap();
        let data = std::fs::read(&p).unwrap();
        assert!(data.iter().any(|&b| b != 0));
        // Not the same bytes twice, and not a pattern.
        assert_ne!(&data[..4096], &data[4096..8192]);
        // The check passes for the right seed ...
        check_wipe(&io, false, seed, &never, &mut reporter(&mut sink)).unwrap();
        // ... and notices a single flipped bit anywhere, and a wrong seed.
        for at in [0usize, 4 << 20, (5 << 20) - 1] {
            let mut bad = data.clone();
            bad[at] ^= 1;
            std::fs::write(&p, &bad).unwrap();
            let e = check_wipe(&io, false, seed, &never, &mut reporter(&mut sink)).unwrap_err();
            assert_eq!(e.kind, ErrorKind::VerifyFailed, "{at}");
        }
        std::fs::write(&p, &data).unwrap();
        assert!(check_wipe(&io, false, seed ^ 1, &never, &mut reporter(&mut sink)).is_err());
    }

    #[test]
    fn a_zero_check_notices_one_stray_byte() {
        let (p, io) = region("strayzero", 3 << 20, 0);
        let mut sink = |_: Event| {};
        let never = AtomicBool::new(false);
        check_wipe(&io, true, 0, &never, &mut reporter(&mut sink)).unwrap();
        let mut d = std::fs::read(&p).unwrap();
        d[(3 << 20) - 7] = 1;
        std::fs::write(&p, d).unwrap();
        let e = check_wipe(&io, true, 0, &never, &mut reporter(&mut sink)).unwrap_err();
        assert!(e.message.contains("did not stick"), "{}", e.message);
    }

    #[test]
    fn cancelling_stops_a_wipe() {
        let (_p, io) = region("cancel", 9 << 20, 0x11);
        let mut sink = |_: Event| {};
        let stop = AtomicBool::new(true);
        let e = wipe_all(&io, Wipe::Zero, 2, true, &stop, &mut reporter(&mut sink)).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Cancelled);
    }

    #[test]
    fn format_with_a_checked_multi_pass_wipe_reports_it() {
        let d = dir("fmtwipe");
        let img = d.join("a.img");
        let o = FormatOptions { parts: vec![one(Fs::Fat32, None, "WIPED")], scheme: Scheme::Mbr, wipe: Wipe::Random, wipe_passes: 2, verify_wipe: true, ..Default::default() };
        let rep = run(&img, 64 << 20, o);
        assert!(rep.notes.iter().any(|n| n.contains("read back and matches")), "{:?}", rep.notes);
        assert!(rep.notes.iter().any(|n| n.contains("overwritten 2 times")), "{:?}", rep.notes);
        // The old wipe data is gone from the partition table area, and the stick is a valid FAT32.
        let head = read_at(&img, 0, 512);
        assert_eq!(&head[510..512], &[0x55, 0xAA]);
    }
}
