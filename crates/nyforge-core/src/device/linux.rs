//! Linux drive discovery from sysfs, /proc/self/mountinfo and /proc/swaps.
//!
//! No udev, no lsblk, no D-Bus: this works in containers, initramfs rescue
//! shells and minimal distros alike.  If udev data exists we use it for labels,
//! otherwise we peek at the superblocks ourselves.

use super::fsprobe;
use super::{Bus, Drive, Partition, Safety};
use crate::error::{NyError, Result};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

pub struct SysPaths {
    pub sys: PathBuf,
    pub dev: PathBuf,
    pub udev: PathBuf,
}

impl SysPaths {
    pub fn real() -> Self {
        Self { sys: "/sys".into(), dev: "/dev".into(), udev: "/run/udev/data".into() }
    }
}

fn read_trim(p: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

fn read_u64(p: impl AsRef<Path>) -> Option<u64> {
    read_trim(p).and_then(|s| s.parse().ok())
}

pub fn list_drives() -> Result<Vec<Drive>> {
    let mountinfo = fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    let swaps = fs::read_to_string("/proc/swaps").unwrap_or_default();
    Ok(list_with(&SysPaths::real(), &mountinfo, &swaps, true))
}

#[derive(Debug)]
struct Mount {
    devno: String,
    mountpoint: String,
    source: String,
}

fn unescape_mount(s: &str) -> String {
    // mountinfo escapes space, tab, newline and backslash as \040 \011 \012 \134
    let mut out = String::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' {
            if let Some(oct) = s.get(i + 1..i + 4) {
                if let Ok(v) = u8::from_str_radix(oct, 8) {
                    out.push(v as char);
                    i += 4;
                    continue;
                }
            }
        }
        // copy one whole UTF-8 character
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn parse_mountinfo(text: &str) -> Vec<Mount> {
    let mut v = Vec::new();
    for line in text.lines() {
        let Some((pre, post)) = line.split_once(" - ") else { continue };
        let f: Vec<&str> = pre.split(' ').collect();
        if f.len() < 5 {
            continue;
        }
        let p: Vec<&str> = post.split(' ').collect();
        if p.len() < 2 {
            continue;
        }
        v.push(Mount { devno: f[2].to_string(), mountpoint: unescape_mount(f[4]), source: unescape_mount(p[1]) });
    }
    v
}

fn is_system_mountpoint(mp: &str) -> bool {
    mp == "/"
        || mp == "/boot"
        || mp.starts_with("/boot/")
        || mp == "/efi"
        || ["/usr", "/var", "/home", "/nix", "/opt", "/srv", "/etc", "/run/initramfs", "/sysroot"].contains(&mp)
        || mp.starts_with("/var/")
        || mp.starts_with("/usr/")
}

struct Topology {
    /// "8:17" -> "sdb1"
    devno_to_name: HashMap<String, String>,
    /// "sdb1" -> "sdb"
    part_parent: HashMap<String, String>,
    sys_block: PathBuf,
}

impl Topology {
    fn build(paths: &SysPaths) -> Self {
        let sys_block = paths.sys.join("block");
        let mut devno_to_name = HashMap::new();
        let mut part_parent = HashMap::new();
        if let Ok(rd) = fs::read_dir(&sys_block) {
            for e in rd.flatten() {
                let disk = e.file_name().to_string_lossy().into_owned();
                if let Some(d) = read_trim(sys_block.join(&disk).join("dev")) {
                    devno_to_name.insert(d, disk.clone());
                }
                if let Ok(children) = fs::read_dir(sys_block.join(&disk)) {
                    for c in children.flatten() {
                        let name = c.file_name().to_string_lossy().into_owned();
                        if c.path().join("partition").exists() {
                            part_parent.insert(name.clone(), disk.clone());
                            if let Some(d) = read_trim(c.path().join("dev")) {
                                devno_to_name.insert(d, name);
                            }
                        }
                    }
                }
            }
        }
        Self { devno_to_name, part_parent, sys_block }
    }

    fn slaves(&self, name: &str) -> Vec<String> {
        let disk = self.part_parent.get(name).map(|s| s.as_str()).unwrap_or(name);
        let p = if disk == name {
            self.sys_block.join(name).join("slaves")
        } else {
            self.sys_block.join(disk).join(name).join("slaves")
        };
        fs::read_dir(p)
            .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default()
    }

    /// Physical disks that ultimately back `name` (through partitions, LVM, LUKS, RAID).
    fn base_disks(&self, name: &str, depth: u8, out: &mut HashSet<String>) {
        if depth > 8 {
            return;
        }
        if let Some(parent) = self.part_parent.get(name) {
            // A partition might itself sit on a dm/md parent; keep resolving.
            self.base_disks(parent, depth + 1, out);
            return;
        }
        let s = self.slaves(name);
        if s.is_empty() {
            out.insert(name.to_string());
        } else {
            for x in s {
                self.base_disks(&x, depth + 1, out);
            }
        }
    }

    /// Partitions (not disks) that back `name`; used to show "mounted" status.
    fn base_parts(&self, name: &str, depth: u8, out: &mut HashSet<String>) {
        if depth > 8 {
            return;
        }
        let s = self.slaves(name);
        if s.is_empty() {
            out.insert(name.to_string());
        } else {
            for x in s {
                self.base_parts(&x, depth + 1, out);
            }
        }
    }
}

fn resolve_mount_name(paths: &SysPaths, topo: &Topology, m: &Mount) -> Option<String> {
    if m.source.starts_with("/dev/") {
        let rel = m.source.trim_start_matches("/dev/");
        let real = paths.dev.join(rel);
        let canon = fs::canonicalize(&real).unwrap_or(real);
        if let Some(n) = canon.file_name() {
            return Some(n.to_string_lossy().into_owned());
        }
    }
    topo.devno_to_name.get(&m.devno).cloned()
}

fn bus_of(paths: &SysPaths, name: &str) -> Bus {
    let dir = paths.sys.join("block").join(name);
    let canon = fs::canonicalize(&dir).unwrap_or(dir.clone());
    let c = canon.to_string_lossy();
    if name.starts_with("loop") || name.starts_with("nbd") || name.starts_with("ram") || name.starts_with("zram") {
        return Bus::Virtual;
    }
    if c.contains("/usb") {
        return Bus::Usb;
    }
    if name.starts_with("mmcblk") {
        return match read_trim(dir.join("device/type")).as_deref() {
            Some("SD") | Some("SDIO") => Bus::Sd,
            _ => Bus::Mmc,
        };
    }
    if name.starts_with("nvme") {
        return Bus::Nvme;
    }
    if c.contains("/virtio") || name.starts_with("vd") || name.starts_with("xvd") {
        return Bus::Virtual;
    }
    if c.contains("/ata") || name.starts_with("sd") || name.starts_with("hd") {
        return Bus::Sata;
    }
    Bus::Unknown
}

fn udev_props(paths: &SysPaths, devno: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    if let Ok(t) = fs::read_to_string(paths.udev.join(format!("b{devno}"))) {
        for l in t.lines() {
            if let Some(kv) = l.strip_prefix("E:") {
                if let Some((k, v)) = kv.split_once('=') {
                    m.insert(k.to_string(), v.to_string());
                }
            }
        }
    }
    m
}

fn skip_name(n: &str) -> bool {
    ["ram", "zram", "sr", "fd", "dm-", "md", "nbd", "zd", "bcache", "mtdblock"].iter().any(|p| n.starts_with(p))
}

pub(crate) fn list_with(paths: &SysPaths, mountinfo: &str, swaps: &str, probe: bool) -> Vec<Drive> {
    let topo = Topology::build(paths);
    let mounts = parse_mountinfo(mountinfo);

    // Which disks hold the running system?
    let mut system_disks: HashSet<String> = HashSet::new();
    let mut part_mounts: HashMap<String, Vec<String>> = HashMap::new();
    for m in &mounts {
        let Some(name) = resolve_mount_name(paths, &topo, m) else { continue };
        let mut parts = HashSet::new();
        topo.base_parts(&name, 0, &mut parts);
        for p in parts {
            part_mounts.entry(p).or_default().push(m.mountpoint.clone());
        }
        if is_system_mountpoint(&m.mountpoint) {
            topo.base_disks(&name, 0, &mut system_disks);
        }
    }
    for line in swaps.lines().skip(1) {
        if let Some(dev) = line.split_whitespace().next() {
            if let Some(rel) = dev.strip_prefix("/dev/") {
                let real = paths.dev.join(rel);
                let canon = fs::canonicalize(&real).unwrap_or(real);
                if let Some(n) = canon.file_name() {
                    topo.base_disks(&n.to_string_lossy(), 0, &mut system_disks);
                }
            }
        }
    }

    let mut drives = Vec::new();
    let Ok(rd) = fs::read_dir(paths.sys.join("block")) else { return drives };
    let mut names: Vec<String> = rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    for name in names {
        if skip_name(&name) {
            continue;
        }
        let dir = paths.sys.join("block").join(&name);
        let sectors = read_u64(dir.join("size")).unwrap_or(0);
        let size = sectors * 512;
        let bus = bus_of(paths, &name);
        let removable = read_trim(dir.join("removable")).as_deref() == Some("1");
        let read_only = read_trim(dir.join("ro")).as_deref() == Some("1");
        let sector_size = read_u64(dir.join("queue/logical_block_size")).unwrap_or(512) as u32;
        let vendor = read_trim(dir.join("device/vendor")).unwrap_or_default();
        let model = read_trim(dir.join("device/model"))
            .or_else(|| read_trim(dir.join("device/name")))
            .unwrap_or_default();
        let devno = read_trim(dir.join("dev")).unwrap_or_default();
        let props = udev_props(paths, &devno);
        let serial = props
            .get("ID_SERIAL_SHORT")
            .cloned()
            .or_else(|| read_trim(dir.join("device/serial")))
            .filter(|s| !s.is_empty());

        let is_system = system_disks.contains(&name);
        let safety = if is_system {
            Safety::System
        } else if bus == Bus::Virtual {
            Safety::Virtual
        } else if bus == Bus::Usb || bus == Bus::Sd || (removable && bus != Bus::Nvme) {
            Safety::Safe
        } else {
            Safety::Internal
        };

        let mut partitions = Vec::new();
        if let Ok(children) = fs::read_dir(&dir) {
            for c in children.flatten() {
                let pname = c.file_name().to_string_lossy().into_owned();
                let pp = c.path();
                if !pname.starts_with(&name) || !pp.join("partition").exists() {
                    continue;
                }
                let number = read_u64(pp.join("partition")).unwrap_or(0) as u32;
                let psize = read_u64(pp.join("size")).unwrap_or(0) * 512;
                let pdev = read_trim(pp.join("dev")).unwrap_or_default();
                let pprops = udev_props(paths, &pdev);
                let (mut fstype, mut label) = (pprops.get("ID_FS_TYPE").cloned(), pprops.get("ID_FS_LABEL").cloned());
                if fstype.is_none() && probe && !is_system {
                    let info = fsprobe::probe_path(&paths.dev.join(&pname));
                    fstype = info.fstype;
                    label = label.or(info.label);
                }
                let mut mps = part_mounts.get(&pname).cloned().unwrap_or_default();
                mps.sort();
                mps.dedup();
                partitions.push(Partition {
                    path: paths.dev.join(&pname).to_string_lossy().into_owned(),
                    number,
                    size: psize,
                    fstype,
                    label,
                    mountpoints: mps,
                });
            }
        }
        partitions.sort_by_key(|p| p.number);
        // A file system straight on the disk (no partition table, common on old sticks and SD cards): show it as
        // "partition 0" so that its mount points are known and the drive is not mistaken for an idle one.
        if partitions.is_empty() {
            if let Some(mps) = part_mounts.get(&name) {
                let (mut fstype, mut label) = (props.get("ID_FS_TYPE").cloned(), props.get("ID_FS_LABEL").cloned());
                if fstype.is_none() && probe && !is_system {
                    let info = fsprobe::probe_path(&paths.dev.join(&name));
                    fstype = info.fstype;
                    label = label.or(info.label);
                }
                let mut mps = mps.clone();
                mps.sort();
                mps.dedup();
                partitions.push(Partition { path: paths.dev.join(&name).to_string_lossy().into_owned(), number: 0, size, fstype, label, mountpoints: mps });
            }
        }

        drives.push(Drive {
            path: paths.dev.join(&name).to_string_lossy().into_owned(),
            vendor,
            model,
            serial,
            size,
            sector_size,
            bus,
            removable,
            read_only,
            safety,
            partitions,
        });
    }
    drives
}

// ------------------------------------------------------------ privileged bits

/// Unmount every mounted partition of `drive`. Returns what was unmounted.
pub fn unmount_drive(drive: &Drive) -> Result<Vec<String>> {
    use std::ffi::CString;
    let mut done = Vec::new();
    // Deepest mountpoints first so nested mounts come off cleanly.
    let mut mps: Vec<String> = drive.mounted_points().iter().map(|s| s.to_string()).collect();
    mps.sort_by_key(|m| std::cmp::Reverse(m.len()));
    for mp in mps {
        let c = CString::new(mp.clone()).map_err(|_| NyError::other("odd mountpoint"))?;
        let r = unsafe { libc::umount2(c.as_ptr(), 0) };
        if r != 0 {
            let e = std::io::Error::last_os_error();
            // EINVAL: not a mount point any more (something else got there first, or it was unmounted a moment ago).
            if e.raw_os_error() == Some(libc::EINVAL) {
                continue;
            }
            return Err(NyError::from_io(&e, &format!("unmounting {mp}")));
        }
        done.push(mp);
    }
    Ok(done)
}

/// Ask the kernel to re-read the partition table (so new partitions appear).
pub fn reread_partition_table(file: &fs::File) {
    use std::os::fd::AsRawFd;
    const BLKRRPART: libc::Ioctl = 0x125F;
    unsafe {
        libc::ioctl(file.as_raw_fd(), BLKRRPART);
    }
}

/// Does the device implement fast zeroing (so skipping zero chunks is both safe and quick)?
pub fn write_zeroes_supported(sys: &Path, name: &str) -> bool {
    read_u64(sys.join("block").join(name).join("queue/write_zeroes_max_bytes")).unwrap_or(0) > 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn mk(dir: &Path, rel: &str, content: &str) {
        let p = dir.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    /// Build a fake machine: nvme0n1 (system, with LVM root), sda (internal data),
    /// sdb (USB stick with a FAT partition) and mmcblk0 (SD card).
    fn fake_machine() -> (PathBuf, SysPaths) {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "nyforge-fake-sys-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&root);
        let sys = root.join("sys");
        let dev = root.join("dev");
        fs::create_dir_all(&dev).unwrap();

        // helper to create a disk whose real location is under devices/<hw> and symlink from block/
        let disk = |name: &str, hw: &str, sectors: u64, removable: &str, dno: &str| {
            let real = sys.join("devices").join(hw).join("block").join(name);
            mk(&real, "size", &sectors.to_string());
            mk(&real, "removable", removable);
            mk(&real, "ro", "0");
            mk(&real, "dev", dno);
            mk(&real, "queue/logical_block_size", "512");
            fs::create_dir_all(sys.join("block")).unwrap();
            symlink(&real, sys.join("block").join(name)).unwrap();
            fs::write(dev.join(name), b"").unwrap();
            real
        };
        let part = |real: &Path, name: &str, n: u32, sectors: u64, dno: &str| {
            let p = real.join(name);
            mk(&p, "partition", &n.to_string());
            mk(&p, "size", &sectors.to_string());
            mk(&p, "dev", dno);
            fs::write(dev.join(name), b"").unwrap();
        };

        let nvme = disk("nvme0n1", "pci0000:00/0000:00:1d.0/nvme/nvme0", 1_000_215_216, "0", "259:0");
        part(&nvme, "nvme0n1p1", 1, 1_048_576, "259:1");
        part(&nvme, "nvme0n1p2", 2, 999_000_000, "259:2");

        let sda = disk("sda", "pci0000:00/0000:00:17.0/ata1/host0/target0:0:0/0:0:0:0", 2_000_000_000, "0", "8:0");
        part(&sda, "sda1", 1, 1_999_000_000, "8:1");

        let sdb = disk("sdb", "pci0000:00/0000:00:14.0/usb1/1-2/1-2:1.0/host6/target6:0:0/6:0:0:0", 30_000_000, "1", "8:16");
        mk(&sdb, "device/vendor", "SanDisk ");
        mk(&sdb, "device/model", "Ultra            ");
        part(&sdb, "sdb1", 1, 29_999_000, "8:17");
        // a FAT32 superblock for the probe
        let mut fat = vec![0u8; 0x8200];
        fat[0x52..0x57].copy_from_slice(b"FAT32");
        fat[0x47..0x47 + 11].copy_from_slice(b"HOLIDAY    ");
        fs::write(dev.join("sdb1"), fat).unwrap();

        let mmc = disk("mmcblk0", "pci0000:00/0000:00:1a.0/mmc_host/mmc0/mmc0:aaaa", 62_333_952, "0", "179:0");
        mk(&mmc, "device/type", "SD");

        // LVM: dm-0 sits on nvme0n1p2 and is mounted at /
        let dm = disk("dm-0", "virtual", 998_000_000, "0", "253:0");
        mk(&dm, "slaves/nvme0n1p2", "");
        fs::create_dir_all(sys.join("block/dm-0/slaves")).ok();
        let _ = dm;

        let paths = SysPaths { sys: sys.clone(), dev: dev.clone(), udev: root.join("udev") };
        (root, paths)
    }

    const MOUNTINFO: &str = "\
24 1 253:0 / / rw,relatime - ext4 /dev/dm-0 rw
30 24 259:1 / /boot/efi rw - vfat /dev/nvme0n1p1 rw
41 24 8:17 / /run/media/me/HOLIDAY rw - vfat /dev/sdb1 rw
";

    #[test]
    fn classifies_a_realistic_machine() {
        let (root, paths) = fake_machine();
        let drives = list_with(&paths, MOUNTINFO, "Filename\tType\tSize\tUsed\tPriority\n", true);
        let by = |n: &str| drives.iter().find(|d| d.path.ends_with(&format!("/{n}"))).unwrap_or_else(|| panic!("no {n}"));

        assert_eq!(by("nvme0n1").safety, Safety::System, "LVM root must resolve to its physical disk");
        assert_eq!(by("sda").safety, Safety::Internal);
        assert_eq!(by("sdb").safety, Safety::Safe);
        assert_eq!(by("sdb").bus, Bus::Usb);
        assert_eq!(by("sdb").display_name(), "SanDisk Ultra");
        assert_eq!(by("mmcblk0").bus, Bus::Sd);
        assert_eq!(by("mmcblk0").safety, Safety::Safe);

        let usb = by("sdb");
        assert_eq!(usb.partitions.len(), 1);
        assert_eq!(usb.partitions[0].label.as_deref(), Some("HOLIDAY"));
        assert_eq!(usb.partitions[0].mountpoints, vec!["/run/media/me/HOLIDAY".to_string()]);
        assert!(usb.contents_summary().contains("HOLIDAY"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn swap_partition_marks_disk_as_system() {
        let (root, paths) = fake_machine();
        let swaps = "Filename\tType\tSize\tUsed\tPriority\n/dev/sda1 partition 1000 0 -2\n";
        let drives = list_with(&paths, "", swaps, false);
        let sda = drives.iter().find(|d| d.path.ends_with("/sda")).unwrap();
        assert_eq!(sda.safety, Safety::System);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn mountinfo_escapes() {
        let m = parse_mountinfo("41 24 8:17 / /run/media/me/My\\040Stick rw - vfat /dev/sdb1 rw\n");
        assert_eq!(m[0].mountpoint, "/run/media/me/My Stick");
    }
}
