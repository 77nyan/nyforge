//! Creating the filesystems.
//!
//! FAT16, FAT32 and exFAT are written by NyForge itself, so they work the same on every platform and
//! without any helper program. ext4 and NTFS are far bigger formats: NyForge runs the system's own
//! `mkfs` for them (Linux only), either on the new partition's device node or, for image files, on a
//! temporary image that is copied into place.

use super::{Fs, PartSpec};
use crate::error::{ErrorKind, NyError, Result};
use crate::nyboot::exfat::{ExFat, FormatParams};
use crate::nyboot::io::{FileIo, Io};
use crate::nyboot::stream::CachedStream;
use fatfs::{FatType, FileSystem, FormatVolumeOptions, FsOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const SECTOR: u64 = 512;

fn fat_err<E: std::fmt::Display>(e: E) -> NyError {
    NyError::other(format!("Could not write the filesystem: {e}"))
}

/// What the volume label of this filesystem may contain. Returns the label as it will be stored.
pub fn validate_label(fs: Fs, label: &str) -> Result<String> {
    match fs {
        Fs::Fat16 | Fs::Fat32 => {
            let up = label.to_uppercase();
            if up.chars().count() > 11 {
                return Err(NyError::invalid(format!("A {} label can have at most 11 characters (\"{label}\" has {}).", fs.name(), label.chars().count())));
            }
            if let Some(c) = up.chars().find(|c| !(c.is_ascii_alphanumeric() || " !#$%&'()-@^_`{}~".contains(*c))) {
                return Err(NyError::invalid(format!("\"{c}\" cannot be used in a {} label.", fs.name()))
                    .with_hint("Use letters A-Z, digits, spaces and ! # $ % & ' ( ) - @ ^ _ ` { } ~ only."));
            }
            Ok(up)
        }
        Fs::ExFat => {
            if label.encode_utf16().count() > 11 {
                return Err(NyError::invalid("An exFAT label can have at most 11 characters."));
            }
            if let Some(c) = label.chars().find(|c| c.is_control() || "\"*/:<>?\\|".contains(*c)) {
                return Err(NyError::invalid(format!("\"{c}\" cannot be used in an exFAT label.")));
            }
            Ok(label.to_string())
        }
        Fs::Ntfs => {
            if label.chars().count() > 32 {
                return Err(NyError::invalid("An NTFS label can have at most 32 characters."));
            }
            if label.chars().any(|c| c.is_control() || c == '"') {
                return Err(NyError::invalid("That NTFS label has characters NTFS does not allow."));
            }
            Ok(label.to_string())
        }
        Fs::Ext2 | Fs::Ext3 | Fs::Ext4 => {
            if label.len() > 16 {
                return Err(NyError::invalid("An ext2/ext3/ext4 label can have at most 16 bytes."));
            }
            if label.chars().any(|c| c.is_control()) {
                return Err(NyError::invalid("That ext label has control characters."));
            }
            Ok(label.to_string())
        }
    }
}

pub fn validate_cluster(fs: Fs, bytes: u64) -> Result<()> {
    if !bytes.is_power_of_two() {
        return Err(NyError::invalid("The cluster size must be a power of two (for example 4K, 32K)."));
    }
    let (lo, hi) = match fs {
        Fs::Fat16 | Fs::Fat32 => (512, 64 << 10),
        Fs::ExFat => (512, 32 << 20),
        Fs::Ntfs => (512, 64 << 10),
        Fs::Ext2 | Fs::Ext3 | Fs::Ext4 => (1024, 4096),
    };
    if bytes < lo || bytes > hi {
        return Err(NyError::invalid(format!(
            "{} clusters can be {} to {} bytes.",
            fs.name(),
            crate::format::human_size(lo),
            crate::format::human_size(hi)
        )));
    }
    Ok(())
}

/// A FAT type is defined by its cluster count: FAT32 needs at least 65 525 clusters, FAT16 between 4 085 and 65 524.
/// A cluster size that cannot reach that for this partition would silently give a different FAT type than the
/// partition table announces, so it is refused up front.
pub fn check_cluster_fits(fs: Fs, partition_bytes: u64, cluster: u64) -> Result<()> {
    let clusters = partition_bytes / cluster;
    let (lo, hi) = match fs {
        Fs::Fat32 => (66_000, u64::MAX),
        Fs::Fat16 => (4_200, 65_000),
        _ => return Ok(()),
    };
    if clusters < lo || clusters > hi {
        let hint = match fs {
            Fs::Fat32 if clusters < lo => format!("For FAT32 on {} use a cluster size of at most {}, or leave it out.", crate::format::human_size(partition_bytes), crate::format::human_size((partition_bytes / lo).next_power_of_two() / 2)),
            Fs::Fat16 if clusters > hi => format!("For FAT16 on {} use a cluster size of at least {}, or leave it out.", crate::format::human_size(partition_bytes), crate::format::human_size((partition_bytes / hi).next_power_of_two())),
            _ => "Leave the cluster size out to let NyForge choose.".to_string(),
        };
        return Err(NyError::invalid(format!(
            "A {} cluster size of {} on {} would give {} clusters, which {} cannot have.",
            fs.name(),
            crate::format::human_size(cluster),
            crate::format::human_size(partition_bytes),
            clusters,
            fs.name()
        ))
        .with_hint(hint));
    }
    Ok(())
}

/// Format one FAT16/FAT32/exFAT partition.
/// `start_sector` is where the partition begins on the disk: FAT keeps that in its boot sector.
pub fn make_native(region: FileIo, spec: &PartSpec, label: &str, start_sector: u64, volume_id: u32) -> Result<()> {
    match spec.fs {
        Fs::ExFat => {
            let mut fs = ExFat::format(region, &FormatParams { label: label.to_string(), serial: volume_id, cluster_bytes: spec.cluster })?;
            fs.sync()?;
            Ok(())
        }
        Fs::Fat16 | Fs::Fat32 => {
            let fat32 = spec.fs == Fs::Fat32;
            let mut cs = CachedStream::new(region);
            let mut vol = [b' '; 11];
            let lab = if label.is_empty() { "NO NAME" } else { label };
            vol[..lab.len()].copy_from_slice(lab.as_bytes());
            let mut o = FormatVolumeOptions::new()
                .fat_type(if fat32 { FatType::Fat32 } else { FatType::Fat16 })
                .volume_label(vol)
                .volume_id(volume_id)
                .fats(2);
            if let Some(c) = spec.cluster {
                o = o.bytes_per_cluster(c as u32);
            }
            fatfs::format_volume(&mut cs, o).map_err(fat_err)?;
            // Tell the boot sector (and FAT32's backup of it at sector 6) where the partition starts:
            // Windows and boot firmware use it.
            let hidden = (start_sector as u32).to_le_bytes();
            let copies: &[u64] = if fat32 { &[0, 6] } else { &[0] };
            for s in copies {
                cs.seek(SeekFrom::Start(s * SECTOR + 0x1C)).map_err(fat_err)?;
                cs.write_all(&hidden).map_err(fat_err)?;
            }
            cs.flush_all().map_err(fat_err)?;
            cs.seek(SeekFrom::Start(0)).map_err(fat_err)?; // fatfs insists on starting at 0
            {
                let fs = FileSystem::new(&mut cs, FsOptions::new()).map_err(fat_err)?;
                let want = if fat32 { FatType::Fat32 } else { FatType::Fat16 };
                if fs.fat_type() != want {
                    return Err(NyError::other(format!("The filesystem came out as {:?} instead of {:?}: the partition size and cluster size do not fit together.", fs.fat_type(), want)));
                }
                let _ = fs.stats(); // fills in FSInfo's free-cluster count
                fs.unmount().map_err(fat_err)?;
            }
            cs.flush_all().map_err(fat_err)?;
            cs.io().sync().map_err(|e| NyError::from_io(&e, "flushing the new filesystem"))?;
            Ok(())
        }
        _ => Err(NyError::other("internal error: not a native filesystem")),
    }
}

// ------------------------------------------------------------------------ external tools

/// Does this filesystem need a helper program?
pub fn needs_tool(fs: Fs) -> bool {
    matches!(fs, Fs::Ext2 | Fs::Ext3 | Fs::Ext4 | Fs::Ntfs)
}

fn tool_candidates(fs: Fs) -> &'static [(&'static str, &'static [&'static str])] {
    match fs {
        Fs::Ext2 => &[("mkfs.ext2", &[]), ("mke2fs", &["-t", "ext2"])],
        Fs::Ext3 => &[("mkfs.ext3", &[]), ("mke2fs", &["-t", "ext3"])],
        Fs::Ext4 => &[("mkfs.ext4", &[]), ("mke2fs", &["-t", "ext4"])],
        Fs::Ntfs => &[("mkfs.ntfs", &[]), ("mkntfs", &[]), ("mkfs.ntfs-3g", &[]), ("mkntfs-3g", &[])],
        _ => &[],
    }
}

fn package_hint(fs: Fs) -> String {
    match fs {
        Fs::Ext2 | Fs::Ext3 | Fs::Ext4 => "Install e2fsprogs.".to_string(),
        _ => {
            let os_release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
            format!("Install {}.", ntfs_package_for(&os_release))
        }
    }
}

/// The package that has `mkntfs` / `mkfs.ntfs` on this distribution, read from `/etc/os-release`. On Arch (CachyOS,
/// Manjaro, EndeavourOS, Omarchy) the `ntfs-3g` package holds only the mount driver: the formatter is in `ntfsprogs`.
/// Debian and Ubuntu put it in `ntfs-3g`; Fedora and openSUSE in `ntfsprogs`.
pub fn ntfs_package_for(os_release: &str) -> &'static str {
    let mut ids: Vec<String> = vec![];
    for line in os_release.lines() {
        if let Some(v) = line.strip_prefix("ID=").or_else(|| line.strip_prefix("ID_LIKE=")) {
            ids.extend(v.trim_matches('"').split_whitespace().map(|w| w.to_ascii_lowercase()));
        }
    }
    let has = |names: &[&str]| ids.iter().any(|i| names.contains(&i.as_str()));
    if has(&["arch", "cachyos", "manjaro", "endeavouros", "arcolinux", "garuda", "artix", "omarchy"]) {
        "ntfsprogs"
    } else if has(&["debian", "ubuntu", "linuxmint", "pop", "raspbian"]) {
        "ntfs-3g"
    } else if has(&["fedora", "rhel", "centos", "opensuse", "suse", "sles", "opensuse-tumbleweed", "opensuse-leap"]) {
        "ntfsprogs"
    } else if has(&["alpine"]) {
        "ntfs-3g-progs"
    } else if has(&["void"]) {
        "ntfs-3g"
    } else {
        "ntfsprogs (on Debian and Ubuntu: ntfs-3g)"
    }
}

fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let mut dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    // sudo and pkexec often strip sbin from PATH, which is exactly where the mkfs tools live.
    for extra in ["/sbin", "/usr/sbin", "/usr/local/sbin", "/usr/bin", "/bin", "/usr/local/bin"] {
        dirs.push(PathBuf::from(extra));
    }
    dirs.into_iter().map(|d| d.join(name)).find(|p| p.is_file())
}

/// Check up front (before anything is erased) that `fs` can be made here.
///
/// * Linux: ext4 and NTFS through the system's `mkfs.ext4` / `mkntfs`.
/// * Windows: NTFS through Windows' own `Format-Volume` (always present); no ext4.
/// * macOS: neither (macOS cannot create them). FAT16, FAT32 and exFAT are made by NyForge itself everywhere.
pub fn check_tool(fs: Fs) -> Result<()> {
    if !needs_tool(fs) {
        return Ok(()); // FAT16, FAT32 and exFAT are written by NyForge itself
    }
    if cfg!(windows) && fs == Fs::Ntfs {
        return Ok(());
    }
    if !cfg!(target_os = "linux") {
        let (os, which) = if cfg!(windows) {
            ("Windows", "ext4 needs Linux")
        } else if cfg!(target_os = "macos") {
            ("macOS", "ext4 and NTFS need Linux (NTFS also works on Windows)")
        } else {
            ("this operating system", "ext4 and NTFS need Linux")
        };
        return Err(NyError::new(ErrorKind::Unavailable, format!("{} cannot be made on {os}.", fs.name()))
            .with_hint(format!("NyForge formats FAT16, FAT32 and exFAT itself on every platform; {which}.")));
    }
    // Test hook (screenshots of the "greyed out" help): pretend the system programs are not installed.
    let hidden = std::env::var_os("NYFORGE_TEST_NO_MKFS").is_some();
    if !hidden && tool_candidates(fs).iter().any(|(n, _)| on_path(n).is_some()) {
        return Ok(());
    }
    Err(NyError::new(ErrorKind::Unavailable, format!("Formatting {} needs the system's {} program, which is not installed.", fs.name(), tool_candidates(fs)[0].0))
        .with_hint(package_hint(fs)))
}

pub struct ToolJob<'a> {
    pub spec: &'a PartSpec,
    pub label: &'a str,
    pub start_sector: u64,
    pub bytes: u64,
    pub owner: Option<(u32, u32)>,
}

/// Where the tool should write.
pub enum ToolTarget<'a> {
    /// The new partition's own device node (a real drive).
    Node(&'a Path),
    /// A region of an image file.
    FileRegion { path: &'a Path, offset: u64 },
    /// Windows: partition `number` of `PhysicalDrive<disk>`, which Windows itself formats.
    WindowsPartition { disk: u32, number: u32 },
}

fn run_tool(fs: Fs, extra: &[String]) -> Result<()> {
    let (name, pre) = tool_candidates(fs)
        .iter()
        .find_map(|(n, pre)| on_path(n).map(|p| (p, *pre)))
        .ok_or_else(|| check_tool(fs).err().unwrap())?;
    let out = Command::new(&name)
        .args(pre)
        .args(extra)
        .env("MKE2FS_SYNC", "0")
        .output()
        .map_err(|e| NyError::from_io(&e, &format!("running {}", name.display())))?;
    if !out.status.success() {
        let mut msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if msg.is_empty() {
            msg = String::from_utf8_lossy(&out.stdout).trim().to_string();
        }
        return Err(NyError::other(format!("{} failed: {}", name.file_name().unwrap().to_string_lossy(), msg.lines().last().unwrap_or("no message"))));
    }
    Ok(())
}

fn ext4_args(job: &ToolJob, offset: Option<u64>, size_k: Option<u64>, dev: &Path) -> Vec<String> {
    let mut a: Vec<String> = vec!["-F".into(), "-q".into(), "-m".into(), "0".into()];
    if !job.label.is_empty() {
        a.extend(["-L".into(), job.label.to_string()]);
    }
    if let Some(c) = job.spec.cluster {
        a.extend(["-b".into(), c.to_string()]);
    }
    let mut ext = vec!["lazy_itable_init=1".to_string(), "lazy_journal_init=0".to_string()];
    if let Some((u, g)) = job.owner {
        ext.push(format!("root_owner={u}:{g}"));
    }
    if let Some(o) = offset {
        ext.push(format!("offset={o}"));
    }
    a.extend(["-E".into(), ext.join(",")]);
    a.push(dev.to_string_lossy().into_owned());
    if let Some(k) = size_k {
        a.push(format!("{k}k"));
    }
    a
}

fn ntfs_args(job: &ToolJob, dev: &Path, with_start: bool) -> Vec<String> {
    let mut a: Vec<String> = vec!["-F".into(), "-Q".into()];
    if !job.label.is_empty() {
        a.extend(["-L".into(), job.label.to_string()]);
    }
    if let Some(c) = job.spec.cluster {
        a.extend(["-c".into(), c.to_string()]);
    }
    if with_start {
        a.extend(["-p".into(), job.start_sector.to_string(), "-s".into(), "512".into()]);
    }
    a.push(dev.to_string_lossy().into_owned());
    a
}

pub fn make_with_tool(job: &ToolJob, target: ToolTarget, scratch_dir: &Path) -> Result<()> {
    check_tool(job.spec.fs)?;
    match (job.spec.fs, target) {
        (fs @ (Fs::Ext2 | Fs::Ext3 | Fs::Ext4), ToolTarget::Node(node)) => run_tool(fs, &ext4_args(job, None, None, node)),
        (fs @ (Fs::Ext2 | Fs::Ext3 | Fs::Ext4), ToolTarget::FileRegion { path, offset }) => {
            run_tool(fs, &ext4_args(job, Some(offset), Some(job.bytes / 1024), path))
        }
        (Fs::Ntfs, ToolTarget::Node(node)) => run_tool(Fs::Ntfs, &ntfs_args(job, node, false)),
        (Fs::Ntfs, ToolTarget::FileRegion { path, offset }) => {
            // mkntfs cannot start in the middle of a file: build the volume in a scratch image, then copy it in.
            let tmp = scratch_dir.join(format!("nyforge-mkntfs-{}-{}.img", std::process::id(), offset));
            let result = (|| -> Result<()> {
                let f = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(&tmp)
                    .map_err(|e| NyError::from_io(&e, "creating a scratch image"))?;
                f.set_len(job.bytes).map_err(|e| NyError::from_io(&e, "sizing the scratch image"))?;
                drop(f);
                run_tool(Fs::Ntfs, &ntfs_args(job, &tmp, true))?;
                copy_into(&tmp, path, offset, job.bytes)
            })();
            let _ = std::fs::remove_file(&tmp);
            result
        }
        (Fs::Ntfs, ToolTarget::WindowsPartition { disk, number }) => run_windows_ntfs(job, disk, number),
        _ => Err(NyError::other("internal error: not a tool filesystem")),
    }
}

// ------------------------------------------------------------------ Windows: NTFS by Format-Volume

/// Waits for Windows to see the partition we just wrote, then formats it. Every value that can vary
/// (disk, partition, label, cluster size) arrives in an environment variable, never inside the script,
/// so a label can contain any character without being able to inject a command.
pub const WINDOWS_NTFS_SCRIPT: &str = r#"$ErrorActionPreference = 'Stop'
$disk = [int]$env:NYF_DISK
$num = [int]$env:NYF_PART
Update-Disk -Number $disk -ErrorAction SilentlyContinue
$p = $null
for ($i = 0; $i -lt 50 -and -not $p; $i++) {
  $p = Get-Partition -DiskNumber $disk -PartitionNumber $num -ErrorAction SilentlyContinue
  if (-not $p) { Start-Sleep -Milliseconds 300 }
}
if (-not $p) { throw 'Windows did not create the partition after the partition table was written.' }
$fv = @{ FileSystem = 'NTFS'; Confirm = $false; Force = $true }
if ($env:NYF_LABEL) { $fv['NewFileSystemLabel'] = $env:NYF_LABEL }
if ($env:NYF_CLUSTER) { $fv['AllocationUnitSize'] = [int]$env:NYF_CLUSTER }
$p | Format-Volume @fv | Out-Null"#;

/// The environment [`WINDOWS_NTFS_SCRIPT`] reads.
pub fn windows_ntfs_env(job: &ToolJob, disk: u32, number: u32) -> Vec<(&'static str, String)> {
    let mut v = vec![("NYF_DISK", disk.to_string()), ("NYF_PART", number.to_string()), ("NYF_LABEL", job.label.to_string())];
    if let Some(c) = job.spec.cluster {
        v.push(("NYF_CLUSTER", c.to_string()));
    }
    v
}

#[cfg(windows)]
fn run_windows_ntfs(job: &ToolJob, disk: u32, number: u32) -> Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    // `diskpart` first: it formats the partition itself and does not need Windows to have made a volume object for
    // it yet (`Format-Volume` fails with "No matching MSFT_Volume objects" on a freshly written partition table).
    let first = match crate::winnative::format_partition(disk, number, Fs::Ntfs, job.label, job.spec.cluster) {
        Ok(()) => return Ok(()),
        Err(e) => e.message,
    };
    let mut cmd = Command::new("powershell");
    cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", WINDOWS_NTFS_SCRIPT]).creation_flags(CREATE_NO_WINDOW);
    for (k, val) in windows_ntfs_env(job, disk, number) {
        cmd.env(k, val);
    }
    let out = cmd.output().map_err(|e| NyError::from_io(&e, "running PowerShell to format the volume"))?;
    if !out.status.success() {
        let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(NyError::other(format!(
            "Windows could not format the NTFS volume. diskpart: {first} PowerShell: {}",
            msg.lines().next().unwrap_or("no message")
        )));
    }
    Ok(())
}

#[cfg(not(windows))]
fn run_windows_ntfs(_job: &ToolJob, _disk: u32, _number: u32) -> Result<()> {
    Err(NyError::other("internal error: Windows formatting requested on another platform"))
}

fn copy_into(src: &Path, dst: &Path, offset: u64, len: u64) -> Result<()> {
    use std::io::Read;
    let mut s = std::fs::File::open(src).map_err(|e| NyError::from_io(&e, "reading the scratch image"))?;
    let d = std::fs::OpenOptions::new().write(true).open(dst).map_err(|e| NyError::from_io(&e, "opening the image file"))?;
    let mut buf = vec![0u8; 1 << 20];
    let mut done = 0u64;
    while done < len {
        let n = ((len - done) as usize).min(buf.len());
        s.read_exact(&mut buf[..n]).map_err(|e| NyError::from_io(&e, "reading the scratch image"))?;
        crate::sys::write_all_at(&d, &buf[..n], offset + done).map_err(|e| NyError::from_io(&e, "writing the image file"))?;
        done += n as u64;
    }
    d.sync_all().map_err(|e| NyError::from_io(&e, "flushing the image file"))?;
    Ok(())
}

/// `/dev/sdb` + 1 = `/dev/sdb1`; `/dev/mmcblk0` + 1 = `/dev/mmcblk0p1`.
pub fn partition_node(disk: &str, number: u32) -> PathBuf {
    let sep = if disk.chars().last().map_or(false, |c| c.is_ascii_digit()) { "p" } else { "" };
    PathBuf::from(format!("{disk}{sep}{number}"))
}

/// The kernel creates partition nodes a moment after it re-reads the table.
pub fn wait_for_node(node: &Path) -> Result<()> {
    for _ in 0..100 {
        if node.exists() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Err(NyError::new(ErrorKind::Busy, format!("The system did not create {} after the partition table was written.", node.display()))
        .with_hint("Unplug and re-plug the drive, then run the format again."))
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_package_with_mkntfs_is_named_per_distribution() {
        assert_eq!(ntfs_package_for("NAME=\"CachyOS Linux\"\nID=cachyos\nID_LIKE=arch\n"), "ntfsprogs");
        assert_eq!(ntfs_package_for("ID=arch\n"), "ntfsprogs");
        assert_eq!(ntfs_package_for("ID=endeavouros\nID_LIKE=\"arch\"\n"), "ntfsprogs");
        assert_eq!(ntfs_package_for("ID=ubuntu\nID_LIKE=debian\n"), "ntfs-3g");
        assert_eq!(ntfs_package_for("ID=linuxmint\nID_LIKE=\"ubuntu debian\"\n"), "ntfs-3g");
        assert_eq!(ntfs_package_for("ID=fedora\n"), "ntfsprogs");
        assert!(ntfs_package_for("").contains("ntfsprogs"));
    }

    use super::*;

    #[test]
    fn filesystems_written_by_nyforge_need_no_outside_tool() {
        // The desktop app asks this for every filesystem it offers; it used to panic for FAT and exFAT.
        for fs in [Fs::Fat16, Fs::Fat32, Fs::ExFat] {
            assert!(!needs_tool(fs));
            assert!(check_tool(fs).is_ok(), "{fs:?}");
        }
    }

    #[test]
    fn windows_ntfs_gets_its_values_from_the_environment_only() {
        let spec = PartSpec { fs: Fs::Ntfs, size: None, label: String::new(), cluster: Some(4096) };
        let job = ToolJob { spec: &spec, label: "Evil'; calc; '", start_sector: 2048, bytes: 1 << 30, owner: None };
        let env = windows_ntfs_env(&job, 3, 1);
        assert!(env.contains(&("NYF_DISK", "3".to_string())));
        assert!(env.contains(&("NYF_PART", "1".to_string())));
        assert!(env.contains(&("NYF_LABEL", "Evil'; calc; '".to_string())));
        assert!(env.contains(&("NYF_CLUSTER", "4096".to_string())));
        // The script is a constant: nothing user-supplied is ever pasted into it.
        assert!(!WINDOWS_NTFS_SCRIPT.contains("Evil"));
        assert!(WINDOWS_NTFS_SCRIPT.contains("Format-Volume"));
        assert!(WINDOWS_NTFS_SCRIPT.contains("$env:NYF_LABEL"));
    }

    #[test]
    fn tool_availability_follows_the_platform() {
        // FAT and exFAT never need a tool; the others are decided by the OS we run on.
        assert!(!needs_tool(Fs::Fat32) && !needs_tool(Fs::ExFat) && !needs_tool(Fs::Fat16));
        if cfg!(windows) {
            assert!(check_tool(Fs::Ntfs).is_ok());
            assert!(check_tool(Fs::Ext4).is_err());
        } else if !cfg!(target_os = "linux") {
            assert!(check_tool(Fs::Ntfs).is_err() && check_tool(Fs::Ext4).is_err());
        }
    }

    #[test]
    fn labels_are_checked_per_filesystem() {
        assert_eq!(validate_label(Fs::Fat32, "my usb").unwrap(), "MY USB");
        assert!(validate_label(Fs::Fat32, "THIS IS TOO LONG").is_err());
        assert!(validate_label(Fs::Fat32, "A*B").is_err());
        assert!(validate_label(Fs::Fat32, "ŻÓŁW").is_err());
        assert_eq!(validate_label(Fs::ExFat, "Ünï Cödé").unwrap(), "Ünï Cödé");
        assert!(validate_label(Fs::ExFat, "a/b").is_err());
        assert!(validate_label(Fs::Ext4, &"x".repeat(17)).is_err());
        assert!(validate_label(Fs::Ntfs, &"x".repeat(33)).is_err());
        assert_eq!(validate_label(Fs::Ntfs, "").unwrap(), "");
    }

    #[test]
    fn cluster_sizes_are_checked() {
        assert!(validate_cluster(Fs::Fat32, 32 << 10).is_ok());
        assert!(validate_cluster(Fs::Fat32, 3000).is_err());
        assert!(validate_cluster(Fs::Fat32, 128 << 10).is_err());
        assert!(validate_cluster(Fs::ExFat, 128 << 10).is_ok());
        assert!(validate_cluster(Fs::Ext4, 8192).is_err());
        assert!(validate_cluster(Fs::Ext4, 4096).is_ok());
    }

    #[test]
    fn partition_node_names() {
        assert_eq!(partition_node("/dev/sdb", 1), PathBuf::from("/dev/sdb1"));
        assert_eq!(partition_node("/dev/mmcblk0", 2), PathBuf::from("/dev/mmcblk0p2"));
        assert_eq!(partition_node("/dev/loop0", 1), PathBuf::from("/dev/loop0p1"));
    }

    #[test]
    fn tool_arguments() {
        let spec = PartSpec { fs: Fs::Ext4, size: None, label: "DATA".into(), cluster: Some(4096) };
        let job = ToolJob { spec: &spec, label: "DATA", start_sector: 2048, bytes: 100 << 20, owner: Some((1000, 1000)) };
        let a = ext4_args(&job, Some(1 << 20), Some(102400), Path::new("/tmp/x.img"));
        assert!(a.windows(2).any(|w| w == ["-L", "DATA"]));
        assert!(a.windows(2).any(|w| w == ["-b", "4096"]));
        assert!(a.iter().any(|x| x.contains("root_owner=1000:1000") && x.contains("offset=1048576")));
        assert_eq!(a.last().unwrap(), "102400k");
        let n = ntfs_args(&job, Path::new("/tmp/n.img"), true);
        assert!(n.windows(2).any(|w| w == ["-p", "2048"]));
    }

    #[test]
    fn ext2_and_ext3_are_known_filesystems() {
        assert_eq!("ext2".parse::<Fs>().unwrap(), Fs::Ext2);
        assert_eq!("EXT3".parse::<Fs>().unwrap(), Fs::Ext3);
        assert!(Fs::Ext2.is_ext() && Fs::Ext3.is_ext() && Fs::Ext4.is_ext() && !Fs::Ntfs.is_ext());
        assert!(needs_tool(Fs::Ext2) && needs_tool(Fs::Ext3));
        assert!(validate_label(Fs::Ext3, &"x".repeat(17)).is_err());
        assert!(validate_cluster(Fs::Ext2, 4096).is_ok());
    }
}
