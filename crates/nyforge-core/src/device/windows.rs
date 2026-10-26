//! Windows drive discovery through PowerShell's Storage module
//! (`Get-Disk`, `Get-Partition`, `Get-Volume`).
//!
//! Windows itself tells us which disk is the system/boot disk, which is more
//! reliable than guessing.  The JSON parser is platform independent so it is
//! unit-tested everywhere; only the process spawn is `cfg(windows)`.

use super::{Bus, Drive, Partition, Safety};
#[allow(unused_imports)]
use crate::error::{ErrorKind, NyError, Result};
use serde::{Deserialize, Deserializer};

fn one_or_many<'de, D, T>(d: D) -> std::result::Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum V<T> {
        Many(Vec<T>),
        One(T),
        Null(()),
    }
    Ok(match V::<T>::deserialize(d)? {
        V::Many(v) => v,
        V::One(x) => vec![x],
        V::Null(()) => vec![],
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WinPart {
    number: Option<u32>,
    size: Option<u64>,
    /// Where the partition starts on the disk, in bytes (to read what is on it when Windows does not know).
    offset: Option<u64>,
    letter: Option<String>,
    label: Option<String>,
    fs: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WinDisk {
    number: u32,
    name: Option<String>,
    size: Option<u64>,
    bus: Option<String>,
    #[serde(default)]
    is_system: bool,
    #[serde(default)]
    is_boot: bool,
    #[serde(default)]
    read_only: bool,
    serial: Option<String>,
    logical_sector: Option<u32>,
    #[serde(default, deserialize_with = "one_or_many")]
    partitions: Vec<WinPart>,
}

fn nonempty(s: Option<String>) -> Option<String> {
    // NUL characters come from PowerShell for "no value" (and leave Rust strings that Windows refuses to open).
    s.map(|x| x.replace('\0', "").trim().to_string()).filter(|x| !x.is_empty())
}

/// A drive letter, or None. PowerShell reports a partition without a letter as the character U+0000 (which is
/// what a partly written drive, such as one you cancelled while flashing, looks like), so only `A`-`Z` counts.
fn drive_letter(s: Option<String>) -> Option<char> {
    let t = nonempty(s)?;
    let mut it = t.chars();
    match (it.next(), it.next()) {
        (Some(c), None) if c.is_ascii_alphabetic() => Some(c.to_ascii_uppercase()),
        _ => None,
    }
}

pub fn parse_get_disk_json(json: &str) -> std::result::Result<Vec<Drive>, String> {
    Ok(parse_with_offsets(json)?.into_iter().map(|(d, _)| d).collect())
}

/// The drives, each with the byte offset of every partition (same order as `Drive::partitions`).
#[cfg_attr(not(windows), allow(dead_code))]
fn parse_with_offsets(json: &str) -> std::result::Result<Vec<(Drive, Vec<Option<u64>>)>, String> {
    let json = json.trim().trim_start_matches('\u{feff}');
    if json.is_empty() {
        return Ok(vec![]);
    }
    let disks: Vec<WinDisk> = match serde_json::from_str::<Vec<WinDisk>>(json) {
        Ok(v) => v,
        Err(_) => vec![serde_json::from_str::<WinDisk>(json).map_err(|e| e.to_string())?],
    };
    Ok(disks
        .into_iter()
        .map(|d| {
            let bus = match d.bus.as_deref().unwrap_or("").to_ascii_lowercase().as_str() {
                "usb" => Bus::Usb,
                "sd" | "mmc" => Bus::Sd,
                "nvme" => Bus::Nvme,
                "sata" | "ata" | "sas" | "scsi" | "raid" => Bus::Sata,
                "virtual" | "file backed virtual" | "spaces" => Bus::Virtual,
                _ => Bus::Unknown,
            };
            let safety = if d.is_system || d.is_boot {
                Safety::System
            } else if bus == Bus::Virtual {
                Safety::Virtual
            } else if bus == Bus::Usb || bus == Bus::Sd {
                Safety::Safe
            } else {
                Safety::Internal
            };
            let offsets: Vec<Option<u64>> = d.partitions.iter().map(|p| p.offset).collect();
            let partitions = d
                .partitions
                .into_iter()
                .map(|p| Partition {
                    path: format!("\\\\.\\PhysicalDrive{}\\Partition{}", d.number, p.number.unwrap_or(0)),
                    number: p.number.unwrap_or(0),
                    size: p.size.unwrap_or(0),
                    fstype: nonempty(p.fs).map(|s| s.to_ascii_lowercase()),
                    label: nonempty(p.label),
                    mountpoints: drive_letter(p.letter).map(|l| vec![format!("{l}:\\")]).unwrap_or_default(),
                })
                .collect();
            let drive = Drive {
                path: format!("\\\\.\\PhysicalDrive{}", d.number),
                vendor: String::new(),
                model: nonempty(d.name).unwrap_or_default(),
                serial: nonempty(d.serial),
                size: d.size.unwrap_or(0),
                sector_size: d.logical_sector.unwrap_or(512),
                bus,
                removable: matches!(bus, Bus::Usb | Bus::Sd),
                read_only: d.read_only,
                safety,
                partitions,
            };
            (drive, offsets)
        })
        .collect())
}

/// Windows only names the filesystems it can mount (FAT, exFAT, NTFS, ReFS). What is on the other partitions, such as
/// the ISO 9660 of a hybrid image or the ext4 of a Linux stick, it leaves blank, so look at the partition's first bytes.
/// `read(disk, offset, len)` returns what it can read (possibly less than `len`) or `None`.
#[cfg_attr(not(windows), allow(dead_code))]
fn identify_unknown(drives: &mut [(Drive, Vec<Option<u64>>)], read: &mut dyn FnMut(&str, u32, u64, usize) -> Option<Vec<u8>>) {
    use super::fsprobe::probe_bytes;
    for (d, offsets) in drives.iter_mut() {
        let (path, sector) = (d.path.clone(), d.sector_size);
        for (p, off) in d.partitions.iter_mut().zip(offsets.iter()) {
            let (None, Some(off)) = (&p.fstype, off) else { continue };
            let Some(buf) = read(&path, sector, *off, 0x10400) else { continue };
            let head = &buf[..buf.len().min(0x8200)];
            let tail = if buf.len() > 0x10000 { &buf[0x10000..] } else { &[][..] };
            let info = probe_bytes(head, tail);
            if info.fstype.is_some() {
                p.fstype = info.fstype;
                if p.label.is_none() {
                    p.label = info.label;
                }
            }
        }
    }
}

/// `PhysicalDrive2`, `\\.\PHYSICALDRIVE2` and `\\?\PhysicalDrive2` all mean `\\.\PhysicalDrive2`.
/// Anything else is returned unchanged.
pub fn normalize_spec(spec: &str) -> String {
    match disk_number(spec) {
        Some(n) => format!("\\\\.\\PhysicalDrive{n}"),
        None => spec.to_string(),
    }
}

/// The disk number in a `PhysicalDriveN` path (any spelling of the prefix and case), if it is one.
pub fn disk_number(path: &str) -> Option<u32> {
    let lower = path.trim().to_ascii_lowercase();
    let rest = lower.strip_prefix("\\\\.\\").or_else(|| lower.strip_prefix("\\\\?\\")).unwrap_or(&lower);
    let n = rest.strip_prefix("physicaldrive")?;
    if n.is_empty() || !n.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    n.parse().ok()
}

pub const POWERSHELL_SCRIPT: &str = r#"$ErrorActionPreference='SilentlyContinue'
$out = @(Get-Disk | ForEach-Object {
  $d = $_
  $parts = @(Get-Partition -DiskNumber $d.Number | ForEach-Object {
    $p = $_
    $v = $null
    if ($p.DriveLetter) { $v = Get-Volume -DriveLetter $p.DriveLetter }
    [pscustomobject]@{ Number=[int]$p.PartitionNumber; Size=[uint64]$p.Size; Offset=[uint64]$p.Offset; Letter=[string]$p.DriveLetter; Label=[string]$v.FileSystemLabel; Fs=[string]$v.FileSystem }
  })
  [pscustomobject]@{ Number=[int]$d.Number; Name=[string]$d.FriendlyName; Size=[uint64]$d.Size; Bus=[string]$d.BusType; IsSystem=[bool]$d.IsSystem; IsBoot=[bool]$d.IsBoot; ReadOnly=[bool]$d.IsReadOnly; Serial=[string]$d.SerialNumber; LogicalSector=[int]$d.LogicalSectorSize; Partitions=$parts }
})
ConvertTo-Json -InputObject $out -Depth 4 -Compress"#;

#[cfg(windows)]
pub fn list_drives() -> Result<Vec<Drive>> {
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", POWERSHELL_SCRIPT])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| NyError::new(ErrorKind::Unavailable, format!("Could not run PowerShell to list drives: {e}")))?;
    if !out.status.success() {
        return Err(NyError::new(
            ErrorKind::Unavailable,
            format!("PowerShell failed while listing drives: {}", String::from_utf8_lossy(&out.stderr).trim()),
        ));
    }
    let mut drives = parse_with_offsets(&String::from_utf8_lossy(&out.stdout))
        .map_err(|e| NyError::other(format!("Could not understand the drive list from Windows: {e}")))?;
    // Needs administrator rights to read a raw disk; without them the partitions simply stay unnamed.
    identify_unknown(&mut drives, &mut |path, sector, off, len| read_raw(path, off, len, sector));
    Ok(drives.into_iter().map(|(d, _)| d).collect())
}

#[cfg(windows)]
fn read_raw(disk: &str, off: u64, len: usize, sector: u32) -> Option<Vec<u8>> {
    use std::os::windows::fs::FileExt;
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ_WRITE: u32 = 3;
    let sector = u64::from(sector.max(512));
    let f = std::fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ_WRITE).open(disk).ok()?;
    // Raw disks only answer whole sectors at sector-aligned places.
    let start = off / sector * sector;
    let want = (off - start + len as u64).div_ceil(sector) * sector;
    let mut buf = vec![0u8; want as usize];
    let mut got = 0usize;
    while got < buf.len() {
        match f.seek_read(&mut buf[got..], start + got as u64) {
            Ok(0) | Err(_) => break,
            Ok(n) => got += n,
        }
    }
    let skip = (off - start) as usize;
    if got <= skip {
        return None;
    }
    buf.truncate(got);
    Some(buf.split_off(skip))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"[
 {"Number":0,"Name":"Samsung SSD 980 PRO 1TB","Size":1000204886016,"Bus":"NVMe","IsSystem":true,"IsBoot":true,"ReadOnly":false,"Serial":"S5GX","LogicalSector":512,
  "Partitions":[{"Number":1,"Size":104857600,"Letter":"","Label":"","Fs":""},{"Number":3,"Size":999000000000,"Letter":"C","Label":"Windows","Fs":"NTFS"}]},
 {"Number":2,"Name":"SanDisk Ultra USB 3.0","Size":30752636928,"Bus":"USB","IsSystem":false,"IsBoot":false,"ReadOnly":false,"Serial":"","LogicalSector":512,
  "Partitions":{"Number":1,"Size":30751000000,"Letter":"E","Label":"HOLIDAY","Fs":"FAT32"}},
 {"Number":3,"Name":"Generic Card Reader","Size":0,"Bus":"USB","IsSystem":false,"IsBoot":false,"ReadOnly":false,"Serial":"","LogicalSector":512,"Partitions":null}
]"#;

    #[test]
    fn parses_windows_disks() {
        let d = parse_get_disk_json(SAMPLE).unwrap();
        assert_eq!(d.len(), 3);
        assert_eq!(d[0].path, "\\\\.\\PhysicalDrive0");
        assert_eq!(d[0].safety, Safety::System);
        assert_eq!(d[1].safety, Safety::Safe);
        assert_eq!(d[1].partitions.len(), 1, "single partition arrives as an object, not an array");
        assert_eq!(d[1].partitions[0].mountpoints, vec!["E:\\".to_string()]);
        assert_eq!(d[1].partitions[0].fstype.as_deref(), Some("fat32"));
        assert_eq!(d[2].partitions.len(), 0);
    }

    #[test]
    fn a_nul_drive_letter_is_not_a_mount_point() {
        // What Windows reported for a USB stick whose flash was cancelled halfway: a partition "letter" of U+0000.
        let j = r#"[{"Number":2,"Name":"SanDisk","Size":8,"Bus":"USB","Partitions":[
            {"Number":1,"Size":1,"Letter":"\u0000","Label":"\u0000","Fs":"\u0000"},
            {"Number":2,"Size":1,"Letter":"?","Label":"","Fs":""},
            {"Number":3,"Size":1,"Letter":"ab","Label":"","Fs":""},
            {"Number":4,"Size":1,"Letter":"f","Label":"X","Fs":"FAT32"}]}]"#;
        let d = parse_get_disk_json(j).unwrap();
        let mp: Vec<_> = d[0].partitions.iter().map(|p| p.mountpoints.clone()).collect();
        assert_eq!(mp, vec![vec![], vec![], vec![], vec!["F:\\".to_string()]]);
        assert_eq!(d[0].partitions[0].label, None);
        assert_eq!(d[0].partitions[0].fstype, None);
        assert!(d[0].mounted_points().iter().all(|m| !m.contains('\0')));
    }

    #[test]
    fn single_disk_object_and_bom() {
        let one = "\u{feff}{\"Number\":4,\"Name\":\"X\",\"Size\":8,\"Bus\":\"SD\",\"Partitions\":[]}";
        let d = parse_get_disk_json(one).unwrap();
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].bus, Bus::Sd);
        assert_eq!(d[0].safety, Safety::Safe);
    }

    #[test]
    fn drives_can_be_named_loosely() {
        assert_eq!(normalize_spec("PhysicalDrive2"), "\\\\.\\PhysicalDrive2");
        assert_eq!(normalize_spec("\\\\.\\PHYSICALDRIVE12"), "\\\\.\\PhysicalDrive12");
        assert_eq!(normalize_spec("\\\\?\\physicaldrive3"), "\\\\.\\PhysicalDrive3");
        assert_eq!(normalize_spec("D:\\images\\a.img"), "D:\\images\\a.img");
        assert_eq!(normalize_spec("PhysicalDrive"), "PhysicalDrive");
        assert_eq!(normalize_spec("PhysicalDrive2x"), "PhysicalDrive2x");
        assert_eq!(disk_number("\\\\.\\PhysicalDrive7"), Some(7));
        assert_eq!(disk_number("/dev/sdb"), None);
    }

    #[test]
    fn partitions_windows_cannot_name_are_identified_from_their_first_bytes() {
        // A hybrid ISO written to a USB stick: partition 1 starts at byte 0 and holds an ISO 9660 volume, partition 2
        // (a FAT EFI image) starts at 1 MiB. Windows names neither; partition 3 is a normal FAT32 it does name.
        let j = r#"[{"Number":2,"Name":"SanDisk","Size":8000000,"Bus":"USB","LogicalSector":512,"Partitions":[
            {"Number":1,"Size":1000000,"Offset":0,"Letter":"","Label":"","Fs":""},
            {"Number":2,"Size":4000,"Offset":1048576,"Letter":"","Label":"","Fs":""},
            {"Number":3,"Size":4000,"Offset":3000000,"Letter":"E","Label":"DATA","Fs":"FAT32"},
            {"Number":4,"Size":4000,"Offset":5000000,"Letter":"","Label":"","Fs":""}]}]"#;
        let mut d = parse_with_offsets(j).unwrap();
        let mut disk = vec![0u8; 6_000_000];
        disk[0x8001..0x8006].copy_from_slice(b"CD001");
        disk[0x8028..0x8028 + 8].copy_from_slice(b"ALPINE  ");
        let efi = 1_048_576;
        disk[efi + 0x52..efi + 0x57].copy_from_slice(b"FAT32");
        disk[efi + 0x47..efi + 0x47 + 11].copy_from_slice(b"EFIBOOT    ");
        let mut asked = vec![];
        identify_unknown(&mut d, &mut |_, _, off, len| {
            asked.push(off);
            let o = off as usize;
            Some(disk[o..(o + len).min(disk.len())].to_vec())
        });
        let p = &d[0].0.partitions;
        assert_eq!((p[0].fstype.as_deref(), p[0].label.as_deref()), (Some("iso9660"), Some("ALPINE")));
        assert_eq!((p[1].fstype.as_deref(), p[1].label.as_deref()), (Some("vfat"), Some("EFIBOOT")));
        assert_eq!(p[2].fstype.as_deref(), Some("fat32"), "what Windows already named is not touched");
        assert_eq!(p[3].fstype, None, "blank space stays unrecognised");
        assert_eq!(asked, vec![0, 1_048_576, 5_000_000], "only unnamed partitions are read");
        // A disk that cannot be read (no administrator rights) changes nothing.
        let mut d = parse_with_offsets(j).unwrap();
        identify_unknown(&mut d, &mut |_, _, _, _| None);
        assert!(d[0].0.partitions.iter().take(2).all(|p| p.fstype.is_none()));
    }

    #[test]
    fn empty_output_is_no_drives() {
        assert!(parse_get_disk_json("  ").unwrap().is_empty());
    }
}
