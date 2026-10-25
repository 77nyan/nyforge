//! macOS drive discovery through `diskutil list -plist` and `diskutil info -plist`.
//!
//! macOS can tell us exactly which physical disk the running system lives on (the
//! volume mounted at `/` and the physical store behind its APFS container), which is
//! more reliable than guessing.  The plist parsing and the classification are
//! platform independent so they are unit-tested everywhere; only the process spawn
//! is `cfg(target_os = "macos")`.
//!
//! The rule is the same as on the other platforms, and it fails closed:
//! * only a disk on the USB bus or an SD card can ever be `Safe`;
//! * a disk that holds (or backs) the running system is `System`, always;
//! * if we cannot work out which disk the system is on, listing drives is an error.

use super::{Bus, Drive, Partition, Safety};
#[allow(unused_imports)]
use crate::error::{ErrorKind, NyError, Result};
use plist::{Dictionary, Value};
use std::collections::{HashMap, HashSet};

/// What `diskutil info -plist <disk or volume>` says, reduced to what we use.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DiskInfo {
    /// `disk4` (or `disk3s1` for a volume)
    pub id: String,
    pub internal: bool,
    pub removable: bool,
    /// "USB", "Secure Digital", "PCI-Express", "Apple Fabric", "SATA", "Disk Image", ...
    pub bus_protocol: String,
    pub is_virtual: bool,
    pub size: u64,
    pub block_size: u32,
    pub media_name: String,
    pub writable: bool,
    /// Physical stores behind an APFS container, e.g. `disk0s2`. Non-empty means "synthesized disk".
    pub apfs_stores: Vec<String>,
    pub parent_whole_disk: Option<String>,
}

/// One partition or APFS volume in `diskutil list`.
#[derive(Debug, Clone, PartialEq)]
pub struct ListPart {
    pub id: String,
    pub size: u64,
    pub content: Option<String>,
    pub volume_name: Option<String>,
    pub mount_point: Option<String>,
}

/// One whole disk in `diskutil list`.
#[derive(Debug, Clone, PartialEq)]
pub struct ListDisk {
    pub id: String,
    pub size: u64,
    pub partitions: Vec<ListPart>,
    /// For a synthesized APFS container: the volumes inside it.
    pub apfs_volumes: Vec<ListPart>,
    /// For a synthesized APFS container: the physical partitions it is built on.
    pub apfs_stores: Vec<String>,
}

// --------------------------------------------------------------------- plist reading

fn root_dict(bytes: &[u8]) -> std::result::Result<Dictionary, String> {
    let v = Value::from_reader_xml(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
    v.into_dictionary().ok_or_else(|| "the top level of the plist is not a dictionary".to_string())
}

fn get_str(d: &Dictionary, k: &str) -> Option<String> {
    d.get(k).and_then(Value::as_string).map(str::to_string).filter(|s| !s.is_empty())
}

fn get_bool(d: &Dictionary, k: &str) -> Option<bool> {
    d.get(k).and_then(Value::as_boolean)
}

fn get_u64(d: &Dictionary, k: &str) -> Option<u64> {
    d.get(k).and_then(|v| v.as_unsigned_integer().or_else(|| v.as_signed_integer().and_then(|i| u64::try_from(i).ok())))
}

fn dict_array<'a>(d: &'a Dictionary, k: &str) -> impl Iterator<Item = &'a Dictionary> + 'a {
    d.get(k).and_then(Value::as_array).into_iter().flat_map(|a| a.iter()).filter_map(Value::as_dictionary)
}

/// `APFSPhysicalStores` is a list of `{DeviceIdentifier: disk0s2}` (list) or `{APFSPhysicalStore: disk0s2}` (info).
fn stores_of(d: &Dictionary) -> Vec<String> {
    dict_array(d, "APFSPhysicalStores")
        .filter_map(|s| get_str(s, "DeviceIdentifier").or_else(|| get_str(s, "APFSPhysicalStore")))
        .collect()
}

fn list_part(d: &Dictionary) -> Option<ListPart> {
    Some(ListPart {
        id: get_str(d, "DeviceIdentifier")?,
        size: get_u64(d, "Size").unwrap_or(0),
        content: get_str(d, "Content"),
        volume_name: get_str(d, "VolumeName"),
        mount_point: get_str(d, "MountPoint"),
    })
}

/// Parse `diskutil list -plist`.
pub fn parse_list(bytes: &[u8]) -> std::result::Result<Vec<ListDisk>, String> {
    let root = root_dict(bytes)?;
    let mut out = Vec::new();
    for d in dict_array(&root, "AllDisksAndPartitions") {
        let Some(id) = get_str(d, "DeviceIdentifier") else { continue };
        out.push(ListDisk {
            id,
            size: get_u64(d, "Size").unwrap_or(0),
            partitions: dict_array(d, "Partitions").filter_map(list_part).collect(),
            apfs_volumes: dict_array(d, "APFSVolumes").filter_map(list_part).collect(),
            apfs_stores: stores_of(d),
        });
    }
    Ok(out)
}

/// Parse `diskutil info -plist`.
pub fn parse_info(bytes: &[u8]) -> std::result::Result<DiskInfo, String> {
    let d = root_dict(bytes)?;
    let id = get_str(&d, "DeviceIdentifier").ok_or("diskutil did not say which disk this is")?;
    Ok(DiskInfo {
        id,
        internal: get_bool(&d, "Internal").unwrap_or(true), // unknown = assume the cautious answer
        removable: get_bool(&d, "RemovableMediaOrExternalDevice").or_else(|| get_bool(&d, "RemovableMedia")).unwrap_or(false),
        bus_protocol: get_str(&d, "BusProtocol").unwrap_or_default(),
        is_virtual: get_str(&d, "VirtualOrPhysical").is_some_and(|s| s.eq_ignore_ascii_case("virtual")),
        size: get_u64(&d, "TotalSize").or_else(|| get_u64(&d, "Size")).unwrap_or(0),
        block_size: get_u64(&d, "DeviceBlockSize").unwrap_or(512) as u32,
        media_name: get_str(&d, "MediaName").or_else(|| get_str(&d, "IORegistryEntryName")).unwrap_or_default(),
        writable: get_bool(&d, "WritableMedia").or_else(|| get_bool(&d, "Writable")).unwrap_or(true),
        apfs_stores: stores_of(&d),
        parent_whole_disk: get_str(&d, "ParentWholeDisk"),
    })
}

// ------------------------------------------------------------------ classification

/// `disk0s2` → `disk0`; `disk4` → `disk4`; anything else unchanged.
pub fn base_disk(id: &str) -> String {
    let rest = id.trim_start_matches("/dev/").trim_start_matches('r');
    match rest.strip_prefix("disk") {
        Some(n) => {
            let digits: String = n.chars().take_while(|c| c.is_ascii_digit()).collect();
            if digits.is_empty() {
                id.to_string()
            } else {
                format!("disk{digits}")
            }
        }
        None => id.to_string(),
    }
}

/// `disk4s2` → 2; a whole disk → 0.
fn part_number(id: &str) -> u32 {
    id.rsplit_once('s').and_then(|(_, n)| n.parse().ok()).unwrap_or(0)
}

fn bus_of(info: &DiskInfo) -> Bus {
    if info.is_virtual {
        return Bus::Virtual;
    }
    match info.bus_protocol.to_ascii_lowercase().as_str() {
        "usb" => Bus::Usb,
        "secure digital" | "sd" | "sdxc" | "mmc" => Bus::Sd,
        "disk image" | "virtual interface" | "virtual" => Bus::Virtual,
        "pci-express" | "pcie" | "nvme" | "apple fabric" => Bus::Nvme,
        "sata" | "sas" | "ata" | "scsi" | "fibre channel" | "firewire" | "raid" => Bus::Sata,
        _ => Bus::Unknown,
    }
}

/// Mount points that mean "this is the running macOS".
pub fn is_system_mount(mp: &str) -> bool {
    mp == "/" || mp.starts_with("/System/") || mp == "/System" || mp == "/private/var/vm" || mp.starts_with("/Library/") || mp == "/Users"
}

/// The whole disks the running system depends on, from `diskutil info` of its volumes.
pub fn system_disks(volume_infos: &[DiskInfo]) -> HashSet<String> {
    let mut s = HashSet::new();
    for i in volume_infos {
        s.insert(base_disk(&i.id));
        if let Some(p) = &i.parent_whole_disk {
            s.insert(base_disk(p));
        }
        for st in &i.apfs_stores {
            s.insert(base_disk(st));
        }
    }
    s
}

fn fstype_of(content: Option<&str>) -> Option<String> {
    let c = content?.to_ascii_lowercase();
    Some(
        match c.as_str() {
            "dos_fat_32" | "windows_fat_32" => "fat32",
            "dos_fat_16" | "windows_fat_16" => "fat16",
            "dos_fat_12" => "fat12",
            "windows_ntfs" => "ntfs",
            "dos_exfat" | "exfat" => "exfat",
            "apple_hfs" => "hfs+",
            "apple_apfs" | "apple_apfs_container" => "apfs",
            "linux_filesystem" | "linux" => "ext4",
            "efi" => "vfat",
            _ => return None,
        }
        .to_string(),
    )
}

fn to_partition(p: &ListPart, fstype: Option<String>) -> Partition {
    Partition {
        path: format!("/dev/{}", p.id),
        number: part_number(&p.id),
        size: p.size,
        fstype,
        label: p.volume_name.clone(),
        mountpoints: p.mount_point.iter().cloned().collect(),
    }
}

/// Turn `diskutil`'s answers into drives. `infos` is `diskutil info` per whole disk id.
pub fn build_drives(list: &[ListDisk], infos: &HashMap<String, DiskInfo>, system: &HashSet<String>) -> Vec<Drive> {
    // Containers (synthesized disks): which physical disk do their volumes belong to?
    let mut volumes_of: HashMap<String, Vec<&ListPart>> = HashMap::new();
    for c in list {
        let synthesized = !c.apfs_stores.is_empty() || infos.get(&c.id).is_some_and(|i| !i.apfs_stores.is_empty());
        if !synthesized {
            continue;
        }
        let mut stores = c.apfs_stores.clone();
        if let Some(i) = infos.get(&c.id) {
            stores.extend(i.apfs_stores.iter().cloned());
        }
        for st in stores {
            volumes_of.entry(base_disk(&st)).or_default().extend(c.apfs_volumes.iter());
        }
    }
    // A disk is also "the system" if anything on it, or in a container on it, is mounted like macOS.
    let mut system = system.clone();
    for d in list {
        let mut mounts = d.partitions.iter().chain(d.apfs_volumes.iter()).chain(volumes_of.get(&d.id).into_iter().flatten().copied());
        if mounts.any(|p| p.mount_point.as_deref().is_some_and(is_system_mount)) {
            system.insert(d.id.clone());
        }
    }

    let mut drives = Vec::new();
    for d in list {
        // No answer from diskutil about this disk: we cannot classify it, so we do not offer it.
        let Some(info) = infos.get(&d.id) else { continue };
        // A synthesized APFS container is not a drive; the physical disk under it is.
        if !d.apfs_stores.is_empty() || !info.apfs_stores.is_empty() {
            continue;
        }
        let bus = bus_of(info);
        let safe_kind = (bus == Bus::Usb && !info.internal) || (bus == Bus::Sd && (info.removable || !info.internal));
        let safety = if system.contains(&d.id) {
            Safety::System
        } else if bus == Bus::Virtual {
            Safety::Virtual
        } else if safe_kind {
            Safety::Safe
        } else {
            Safety::Internal
        };

        let mut partitions: Vec<Partition> = d.partitions.iter().map(|p| to_partition(p, fstype_of(p.content.as_deref()))).collect();
        for v in volumes_of.get(&d.id).into_iter().flatten() {
            partitions.push(to_partition(v, Some("apfs".into())));
        }
        partitions.sort_by_key(|p| (p.number, p.path.clone()));

        let name = info.media_name.trim();
        let model = name.strip_suffix(" Media").unwrap_or(name).trim().to_string();
        drives.push(Drive {
            path: format!("/dev/{}", d.id),
            vendor: String::new(),
            model,
            serial: None,
            size: if info.size > 0 { info.size } else { d.size },
            sector_size: if info.block_size > 0 { info.block_size } else { 512 },
            bus,
            removable: matches!(bus, Bus::Usb | Bus::Sd) && safe_kind,
            read_only: !info.writable,
            safety,
            partitions,
        });
    }
    drives
}

/// `diskutil unmountDisk force /dev/disk4`
pub fn unmount_args(drive_path: &str) -> Vec<String> {
    vec!["unmountDisk".into(), "force".into(), crate::sys::mac_block_path(drive_path)]
}

// ------------------------------------------------------------------------ the process

#[cfg(target_os = "macos")]
fn diskutil(args: &[&str]) -> Result<Vec<u8>> {
    use std::process::Command;
    let out = Command::new("/usr/sbin/diskutil")
        .args(args)
        .output()
        .map_err(|e| NyError::new(ErrorKind::Unavailable, format!("Could not run diskutil: {e}")))?;
    if !out.status.success() {
        return Err(NyError::new(
            ErrorKind::Unavailable,
            format!("diskutil {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()),
        ));
    }
    Ok(out.stdout)
}

#[cfg(target_os = "macos")]
pub fn list_drives() -> Result<Vec<Drive>> {
    let bad = |what: &str, e: String| NyError::other(format!("Could not understand diskutil's {what}: {e}"));
    let list = parse_list(&diskutil(&["list", "-plist"])?).map_err(|e| bad("drive list", e))?;

    let mut infos = HashMap::new();
    for d in &list {
        if let Ok(raw) = diskutil(&["info", "-plist", &d.id]) {
            if let Ok(i) = parse_info(&raw) {
                infos.insert(d.id.clone(), i);
            }
        }
    }

    // Which disks is the running system built on? Ask about the root volume and its siblings.
    let mut volume_infos = Vec::new();
    for mp in ["/", "/System/Volumes/Data", "/System/Volumes/Preboot", "/System/Volumes/VM", "/System/Volumes/Update"] {
        if let Ok(raw) = diskutil(&["info", "-plist", mp]) {
            if let Ok(i) = parse_info(&raw) {
                volume_infos.push(i);
            }
        }
    }
    let system = system_disks(&volume_infos);
    if system.is_empty() {
        // Fail closed: if we cannot say where macOS lives we must not offer anything for erasing.
        return Err(NyError::new(ErrorKind::Unavailable, "Could not work out which disk macOS is running from, so no drive can be offered safely."));
    }
    Ok(build_drives(&list, &infos, &system))
}

/// Release every volume on `drive` (including those inside APFS containers on it).
#[cfg(target_os = "macos")]
pub fn unmount_disk(drive: &Drive) -> Result<Vec<String>> {
    use std::process::Command;
    let args = unmount_args(&drive.path);
    let out = Command::new("/usr/sbin/diskutil")
        .args(&args)
        .output()
        .map_err(|e| NyError::new(ErrorKind::Unavailable, format!("Could not run diskutil: {e}")))?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(NyError::new(ErrorKind::Busy, format!("macOS would not release {}: {why}", drive.path))
            .with_hint("Close Finder windows and any app using the drive, then try again."));
    }
    Ok(drive.mounted_points().iter().map(|s| s.to_string()).collect())
}

// ------------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    // ---- a tiny XML plist writer for fixtures
    fn wrap(body: &str) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">{body}</plist>\n"
        )
    }
    fn dict(items: &[String]) -> String {
        format!("<dict>{}</dict>", items.concat())
    }
    fn s(k: &str, v: &str) -> String {
        let v = v.replace('&', "&amp;").replace('<', "&lt;");
        format!("<key>{k}</key><string>{v}</string>")
    }
    fn b(k: &str, v: bool) -> String {
        format!("<key>{k}</key><{}/>", if v { "true" } else { "false" })
    }
    fn n(k: &str, v: u64) -> String {
        format!("<key>{k}</key><integer>{v}</integer>")
    }
    fn arr(k: &str, items: &[String]) -> String {
        format!("<key>{k}</key><array>{}</array>", items.concat())
    }

    fn part(id: &str, content: &str, size: u64, name: Option<&str>, mount: Option<&str>) -> String {
        let mut v = vec![s("Content", content), s("DeviceIdentifier", id), n("Size", size)];
        if let Some(x) = name {
            v.push(s("VolumeName", x));
        }
        if let Some(x) = mount {
            v.push(s("MountPoint", x));
        }
        dict(&v)
    }

    /// An Apple Silicon laptop: internal disk0 (APFS container disk3 holds macOS), a USB stick disk4
    /// with one FAT32 volume, a mounted disk image disk5.
    fn list_fixture() -> String {
        let disk0 = dict(&[
            s("Content", "GUID_partition_scheme"),
            s("DeviceIdentifier", "disk0"),
            arr(
                "Partitions",
                &[
                    part("disk0s1", "Apple_APFS_ISC", 524_288_000, None, None),
                    part("disk0s2", "Apple_APFS", 494_384_795_648, None, None),
                    part("disk0s3", "Apple_APFS_Recovery", 5_368_664_064, None, None),
                ],
            ),
            n("Size", 500_277_792_768),
        ]);
        let disk3 = dict(&[
            arr("APFSPhysicalStores", &[dict(&[s("DeviceIdentifier", "disk0s2")])]),
            arr(
                "APFSVolumes",
                &[
                    part("disk3s1", "APFS", 494_384_795_648, Some("Macintosh HD"), Some("/")),
                    part("disk3s5", "APFS", 494_384_795_648, Some("Data"), Some("/System/Volumes/Data")),
                ],
            ),
            s("Content", "Apple_APFS_Container"),
            s("DeviceIdentifier", "disk3"),
            n("Size", 494_384_795_648),
        ]);
        let disk4 = dict(&[
            s("Content", "FDisk_partition_scheme"),
            s("DeviceIdentifier", "disk4"),
            arr("Partitions", &[part("disk4s1", "DOS_FAT_32", 30_751_000_000, Some("HOLIDAY"), Some("/Volumes/HOLIDAY"))]),
            n("Size", 30_752_636_928),
        ]);
        let disk5 = dict(&[
            s("Content", "GUID_partition_scheme"),
            s("DeviceIdentifier", "disk5"),
            arr("Partitions", &[part("disk5s1", "Apple_HFS", 100_000_000, Some("Installer"), Some("/Volumes/Installer"))]),
            n("Size", 104_857_600),
        ]);
        wrap(&dict(&[arr("AllDisksAndPartitions", &[disk0, disk3, disk4, disk5]), arr("WholeDisks", &[s_only("disk0"), s_only("disk3"), s_only("disk4"), s_only("disk5")])]))
    }
    fn s_only(v: &str) -> String {
        format!("<string>{v}</string>")
    }

    fn info_xml(id: &str, bus: &str, internal: bool, removable: bool, virt: bool, size: u64, name: &str) -> String {
        wrap(&dict(&[
            s("DeviceIdentifier", id),
            b("Internal", internal),
            b("RemovableMediaOrExternalDevice", removable),
            s("BusProtocol", bus),
            s("VirtualOrPhysical", if virt { "Virtual" } else { "Physical" }),
            n("TotalSize", size),
            n("DeviceBlockSize", 512),
            s("MediaName", name),
            b("WritableMedia", true),
        ]))
    }

    fn infos() -> HashMap<String, DiskInfo> {
        let mut m = HashMap::new();
        let mut add = |xml: String| {
            let i = parse_info(xml.as_bytes()).unwrap();
            m.insert(i.id.clone(), i);
        };
        add(info_xml("disk0", "Apple Fabric", true, false, false, 500_277_792_768, "APPLE SSD AP0512Q Media"));
        // The synthesized container: virtual, backed by disk0s2.
        add(wrap(&dict(&[
            s("DeviceIdentifier", "disk3"),
            b("Internal", true),
            s("BusProtocol", "Apple Fabric"),
            s("VirtualOrPhysical", "Virtual"),
            n("TotalSize", 494_384_795_648),
            arr("APFSPhysicalStores", &[dict(&[s("APFSPhysicalStore", "disk0s2")])]),
        ])));
        add(info_xml("disk4", "USB", false, true, false, 30_752_636_928, "SanDisk Ultra Media"));
        add(info_xml("disk5", "Disk Image", false, false, true, 104_857_600, "Apple Disk Image Media"));
        m
    }

    fn root_volume_info() -> DiskInfo {
        parse_info(
            wrap(&dict(&[
                s("DeviceIdentifier", "disk3s1s1"),
                s("ParentWholeDisk", "disk3"),
                b("Internal", true),
                arr("APFSPhysicalStores", &[dict(&[s("APFSPhysicalStore", "disk0s2")])]),
            ]))
            .as_bytes(),
        )
        .unwrap()
    }

    #[test]
    fn parses_the_list() {
        let l = parse_list(list_fixture().as_bytes()).unwrap();
        assert_eq!(l.len(), 4);
        assert_eq!(l[0].id, "disk0");
        assert_eq!(l[0].partitions.len(), 3);
        assert_eq!(l[1].apfs_stores, vec!["disk0s2".to_string()]);
        assert_eq!(l[1].apfs_volumes[0].mount_point.as_deref(), Some("/"));
        assert_eq!(l[2].partitions[0].volume_name.as_deref(), Some("HOLIDAY"));
    }

    #[test]
    fn the_disk_macos_runs_from_is_found_through_apfs() {
        let sys = system_disks(&[root_volume_info()]);
        assert!(sys.contains("disk3"), "the synthesized container");
        assert!(sys.contains("disk0"), "the physical disk under the container");
        assert!(!sys.contains("disk4"));
    }

    #[test]
    fn classifies_a_realistic_mac() {
        let list = parse_list(list_fixture().as_bytes()).unwrap();
        let drives = build_drives(&list, &infos(), &system_disks(&[root_volume_info()]));
        let by = |id: &str| drives.iter().find(|d| d.path == format!("/dev/{id}")).unwrap_or_else(|| panic!("no {id}"));

        assert_eq!(by("disk0").safety, Safety::System, "the internal disk holds macOS");
        assert!(drives.iter().all(|d| d.path != "/dev/disk3"), "an APFS container is not a drive");
        let usb = by("disk4");
        assert_eq!(usb.safety, Safety::Safe);
        assert_eq!(usb.bus, Bus::Usb);
        assert_eq!(usb.display_name(), "SanDisk Ultra");
        assert_eq!(usb.size, 30_752_636_928);
        assert_eq!(usb.partitions.len(), 1);
        assert_eq!(usb.partitions[0].path, "/dev/disk4s1");
        assert_eq!(usb.partitions[0].number, 1);
        assert_eq!(usb.partitions[0].fstype.as_deref(), Some("fat32"));
        assert_eq!(usb.partitions[0].label.as_deref(), Some("HOLIDAY"));
        assert_eq!(usb.partitions[0].mountpoints, vec!["/Volumes/HOLIDAY".to_string()]);
        assert_eq!(by("disk5").safety, Safety::Virtual, "a disk image is not a drive");
        // The internal disk's APFS volumes are listed on it, so "mounted" is truthful.
        assert!(by("disk0").mounted_points().contains(&"/"));
    }

    #[test]
    fn a_usb_disk_that_macos_booted_from_is_the_system() {
        // Booted from the external disk: the root volume's physical store is disk4s2.
        let mut root = root_volume_info();
        root.parent_whole_disk = Some("disk6".into());
        root.apfs_stores = vec!["disk4s2".into()];
        let sys = system_disks(&[root]);
        let list = parse_list(list_fixture().as_bytes()).unwrap();
        let drives = build_drives(&list, &infos(), &sys);
        let usb = drives.iter().find(|d| d.path == "/dev/disk4").unwrap();
        assert_eq!(usb.safety, Safety::System);
    }

    #[test]
    fn anything_mounted_like_macos_is_the_system_even_if_the_lookup_missed_it() {
        // diskutil told us nothing useful about the root volume (empty system set)...
        let mut list = parse_list(list_fixture().as_bytes()).unwrap();
        // ...but a "Data" volume is mounted from disk4's APFS container: that disk is the system.
        list[2].partitions.push(ListPart {
            id: "disk4s2".into(),
            size: 1,
            content: Some("Apple_APFS".into()),
            volume_name: Some("Data".into()),
            mount_point: Some("/System/Volumes/Data".into()),
        });
        let drives = build_drives(&list, &infos(), &HashSet::new());
        assert_eq!(drives.iter().find(|d| d.path == "/dev/disk4").unwrap().safety, Safety::System);
    }

    #[test]
    fn only_usb_and_sd_can_ever_be_safe() {
        let list = parse_list(list_fixture().as_bytes()).unwrap();
        for (bus, internal) in [("PCI-Express", false), ("SATA", false), ("Thunderbolt", false), ("Apple Fabric", true), ("", false), ("USB", true)] {
            let mut m = infos();
            let i = m.get_mut("disk4").unwrap();
            i.bus_protocol = bus.into();
            i.internal = internal;
            let drives = build_drives(&list, &m, &system_disks(&[root_volume_info()]));
            let d = drives.iter().find(|d| d.path == "/dev/disk4").unwrap();
            assert_eq!(d.safety, Safety::Internal, "bus {bus:?} internal={internal} must not be offered");
        }
        // The built-in SD card slot counts as an SD card even though the Mac reports it as internal.
        let mut m = infos();
        let i = m.get_mut("disk4").unwrap();
        i.bus_protocol = "Secure Digital".into();
        i.internal = true;
        i.removable = true;
        let d = build_drives(&list, &m, &system_disks(&[root_volume_info()]));
        let d = d.iter().find(|d| d.path == "/dev/disk4").unwrap();
        assert_eq!((d.safety, d.bus), (Safety::Safe, Bus::Sd));
    }

    #[test]
    fn a_disk_diskutil_cannot_describe_is_not_offered() {
        let list = parse_list(list_fixture().as_bytes()).unwrap();
        let mut m = infos();
        m.remove("disk4");
        let drives = build_drives(&list, &m, &system_disks(&[root_volume_info()]));
        assert!(drives.iter().all(|d| d.path != "/dev/disk4"));
    }

    #[test]
    fn write_protected_media_is_read_only() {
        let list = parse_list(list_fixture().as_bytes()).unwrap();
        let mut m = infos();
        m.get_mut("disk4").unwrap().writable = false;
        let drives = build_drives(&list, &m, &system_disks(&[root_volume_info()]));
        assert!(drives.iter().find(|d| d.path == "/dev/disk4").unwrap().read_only);
    }

    #[test]
    fn missing_keys_take_the_cautious_defaults() {
        let i = parse_info(wrap(&dict(&[s("DeviceIdentifier", "disk9")])).as_bytes()).unwrap();
        assert!(i.internal, "unknown = internal = never offered");
        assert_eq!(bus_of(&i), Bus::Unknown);
    }

    #[test]
    fn bad_plists_are_errors_not_panics() {
        assert!(parse_list(b"not a plist").is_err());
        assert!(parse_info(b"<plist version=\"1.0\"><array/></plist>").is_err());
        assert!(parse_info(wrap(&dict(&[])).as_bytes()).is_err());
    }

    #[test]
    fn ids_and_commands() {
        assert_eq!(base_disk("disk0s2"), "disk0");
        assert_eq!(base_disk("disk12"), "disk12");
        assert_eq!(base_disk("/dev/rdisk4s1"), "disk4");
        assert_eq!(base_disk("sda1"), "sda1");
        assert_eq!(unmount_args("/dev/disk4"), ["unmountDisk", "force", "/dev/disk4"]);
        assert_eq!(unmount_args("/dev/rdisk4"), ["unmountDisk", "force", "/dev/disk4"]);
    }

    #[test]
    fn volume_names_with_xml_characters_survive() {
        let l = wrap(&dict(&[arr(
            "AllDisksAndPartitions",
            &[dict(&[s("DeviceIdentifier", "disk4"), n("Size", 1), arr("Partitions", &[part("disk4s1", "DOS_FAT_32", 1, Some("R&D <2>"), None)])])],
        )]));
        let l = parse_list(l.as_bytes()).unwrap();
        assert_eq!(l[0].partitions[0].volume_name.as_deref(), Some("R&D <2>"));
    }
}
