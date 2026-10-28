//! What the drive is, how it is connected, and what it says about its own health.
//!
//! The connection matters more than people expect: a USB 3 stick in a USB 2 port, or on a poor cable, runs at a tenth of its
//! speed, and every benchmark then blames the stick. SMART and the SD/eMMC registers say what the drive itself knows;
//! most sticks and cards report nothing, which is normal and said plainly.

use super::Subject;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkInfo {
    /// "USB"
    pub kind: String,
    /// Mb/s the link was negotiated at (1.5, 12, 480, 5000, 10000, 20000)
    pub speed_mbps: Option<f64>,
    /// "USB 2.0 High-Speed (480 Mb/s)"
    pub label: String,
    /// About the most a drive can move over this link, in MB/s
    pub ceiling_mbps: Option<f64>,
    /// "UASP" (fast, queued) or "Bulk-only" (older, slower for small files)
    pub protocol: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Smart {
    pub available: bool,
    pub passed: Option<bool>,
    pub temperature_c: Option<i64>,
    pub power_on_hours: Option<u64>,
    /// Percent of the rated life used (SSD/NVMe/eMMC)
    pub wear_percent: Option<u64>,
    pub reallocated: Option<u64>,
    pub pending: Option<u64>,
    pub media_errors: Option<u64>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CardInfo {
    /// "SD" or "MMC"
    pub kind: String,
    pub manufacturer: String,
    pub product: String,
    pub date: String,
    pub serial: String,
    pub revision: String,
    /// eMMC health: "0x01 = 0-10% used"
    pub life_time: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub name: String,
    pub size: u64,
    /// "usb", "sd", "mmc", "nvme", "sata", "virtual", "unknown"
    pub bus: String,
    pub removable: bool,
    /// label → value, in display order
    pub details: Vec<(String, String)>,
    pub link: Option<LinkInfo>,
    pub smart: Option<Smart>,
    pub card: Option<CardInfo>,
}

pub fn usb_label(speed_mbps: f64) -> (String, Option<f64>) {
    let (name, ceiling) = if speed_mbps >= 20000.0 {
        ("USB 3.2 Gen 2x2 (20 Gb/s)", 1800.0)
    } else if speed_mbps >= 10000.0 {
        ("USB 3.1 Gen 2 SuperSpeed+ (10 Gb/s)", 900.0)
    } else if speed_mbps >= 5000.0 {
        ("USB 3.0 SuperSpeed (5 Gb/s)", 420.0)
    } else if speed_mbps >= 480.0 {
        ("USB 2.0 High-Speed (480 Mb/s)", 42.0)
    } else if speed_mbps >= 12.0 {
        ("USB 1.1 Full-Speed (12 Mb/s)", 1.1)
    } else {
        ("USB 1.0 Low-Speed (1.5 Mb/s)", 0.15)
    };
    (name.to_string(), Some(ceiling))
}

/// SD association manufacturer ids (the ones that are well documented).
pub fn sd_manufacturer(id: u32) -> Option<&'static str> {
    Some(match id {
        0x01 => "Panasonic",
        0x02 => "Toshiba / Kioxia",
        0x03 => "SanDisk",
        0x1b => "Samsung",
        0x1d => "ADATA",
        0x27 => "Phison",
        0x28 => "Lexar",
        0x31 => "Silicon Power",
        0x41 => "Kingston",
        0x74 => "Transcend",
        0x76 => "Patriot",
        _ => return None,
    })
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::path::{Path, PathBuf};

    fn read(p: &Path) -> Option<String> {
        std::fs::read_to_string(p).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    }

    pub fn block_name(dev: &Path) -> Option<String> {
        let real = std::fs::canonicalize(dev).ok()?;
        real.file_name().map(|n| n.to_string_lossy().into_owned())
    }

    pub fn usb_link(name: &str) -> Option<LinkInfo> {
        let mut dir = std::fs::canonicalize(format!("/sys/block/{name}")).ok()?;
        let mut protocol = None;
        loop {
            if protocol.is_none() {
                if let Ok(d) = std::fs::read_link(dir.join("driver")) {
                    match d.file_name().and_then(|n| n.to_str()) {
                        Some("uas") => protocol = Some("UASP".to_string()),
                        Some("usb-storage") => protocol = Some("Bulk-only".to_string()),
                        _ => {}
                    }
                }
            }
            if dir.join("speed").is_file() && dir.join("idVendor").is_file() {
                let speed: Option<f64> = read(&dir.join("speed")).and_then(|s| s.parse().ok());
                let (label, ceiling) = speed.map(usb_label).unwrap_or(("USB".to_string(), None));
                return Some(LinkInfo { kind: "USB".into(), speed_mbps: speed, label, ceiling_mbps: ceiling, protocol });
            }
            if !dir.pop() || dir == PathBuf::from("/sys") || dir == PathBuf::from("/") {
                return None;
            }
        }
    }

    pub fn usb_strings(name: &str) -> Vec<(String, String)> {
        let mut out = vec![];
        let Ok(mut dir) = std::fs::canonicalize(format!("/sys/block/{name}")) else { return out };
        loop {
            if dir.join("idVendor").is_file() {
                for (label, file) in [("USB maker", "manufacturer"), ("USB product", "product"), ("USB serial", "serial")] {
                    if let Some(v) = read(&dir.join(file)) {
                        out.push((label.to_string(), v));
                    }
                }
                if let (Some(v), Some(p)) = (read(&dir.join("idVendor")), read(&dir.join("idProduct"))) {
                    out.push(("USB id".to_string(), format!("{v}:{p}")));
                }
                if let Some(v) = read(&dir.join("bMaxPower")) {
                    out.push(("Power draw (max)".to_string(), v));
                }
                break;
            }
            if !dir.pop() || dir == PathBuf::from("/sys") {
                break;
            }
        }
        out
    }

    pub fn card(name: &str) -> Option<CardInfo> {
        if !name.starts_with("mmcblk") {
            return None;
        }
        let d = PathBuf::from(format!("/sys/block/{name}/device"));
        let kind = read(&d.join("type"))?;
        let manf = read(&d.join("manfid")).and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok());
        Some(CardInfo {
            kind,
            manufacturer: manf.map(|m| sd_manufacturer(m).map(String::from).unwrap_or(format!("id 0x{m:02x}"))).unwrap_or_default(),
            product: read(&d.join("name")).unwrap_or_default(),
            date: read(&d.join("date")).unwrap_or_default(),
            serial: read(&d.join("serial")).unwrap_or_default(),
            revision: format!("{} / {}", read(&d.join("hwrev")).unwrap_or_default(), read(&d.join("fwrev")).unwrap_or_default()),
            life_time: read(&d.join("life_time")).map(|l| format!("{l} (each step is 10% of the rated life used; 0x01 = 0-10%)")),
        })
    }

    pub fn rotational(name: &str) -> Option<bool> {
        read(&PathBuf::from(format!("/sys/block/{name}/queue/rotational"))).map(|s| s == "1")
    }
}

/// `smartctl` output as health data. Needs smartmontools and administrator rights; absent or unsupported is reported, not an error.
pub fn smart(dev: &std::path::Path) -> Smart {
    let run = |extra: &[&str]| -> Option<serde_json::Value> {
        let mut cmd = std::process::Command::new("smartctl");
        cmd.arg("-a").arg("-j");
        cmd.args(extra);
        cmd.arg(dev);
        cmd.stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).stdout(std::process::Stdio::piped());
        let child = cmd.spawn().ok()?;
        let pid = child.id();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(child.wait_with_output());
        });
        match rx.recv_timeout(std::time::Duration::from_secs(12)) {
            Ok(Ok(out)) => serde_json::from_slice(&out.stdout).ok(),
            _ => {
                #[cfg(unix)]
                unsafe {
                    libc::kill(pid as i32, libc::SIGKILL);
                }
                let _ = pid;
                None
            }
        }
    };
    let have_tool = std::process::Command::new("smartctl").arg("-V").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().is_ok();
    if !have_tool {
        return Smart { available: false, note: Some("Health data (hours powered on, wear, temperature) needs the smartctl tool from smartmontools, which is not installed. Only USB hard disks and SSDs have such data; most flash sticks and SD cards do not.".into()), ..Default::default() };
    }
    let mut j = run(&[]);
    let has = |j: &Option<serde_json::Value>| j.as_ref().is_some_and(|v| v.get("smart_status").is_some());
    if !has(&j) {
        // USB-to-SATA bridges need to be told what they are
        let k = run(&["-d", "sat"]);
        if has(&k) {
            j = k;
        }
    }
    let Some(v) = j.filter(|v| v.get("smart_status").is_some()) else {
        return Smart { available: false, note: Some("This drive does not report health data such as hours powered on. Almost all USB sticks and SD cards cannot; the surface scan and the capacity check are how NyTest judges them.".into()), ..Default::default() };
    };
    let mut s = Smart { available: true, ..Default::default() };
    s.passed = v["smart_status"]["passed"].as_bool();
    s.temperature_c = v["temperature"]["current"].as_i64();
    s.power_on_hours = v["power_on_time"]["hours"].as_u64();
    if let Some(t) = v["ata_smart_attributes"]["table"].as_array() {
        for a in t {
            let raw = a["raw"]["value"].as_u64();
            match a["id"].as_u64() {
                Some(5) => s.reallocated = raw,
                Some(197) => s.pending = raw,
                Some(177) | Some(231) | Some(233) => {
                    // normalised value counts down from 100
                    if let Some(val) = a["value"].as_u64() {
                        if val <= 100 {
                            s.wear_percent = Some(s.wear_percent.unwrap_or(0).max(100 - val));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let n = &v["nvme_smart_health_information_log"];
    if n.is_object() {
        s.wear_percent = n["percentage_used"].as_u64();
        s.media_errors = n["media_errors"].as_u64();
        s.temperature_c = s.temperature_c.or(n["temperature"].as_i64());
        s.power_on_hours = s.power_on_hours.or(n["power_on_hours"].as_u64());
    }
    s
}

pub fn gather(s: &Subject) -> DeviceInfo {
    let drive = s.drive.as_ref();
    let name = drive.map(|d| d.display_name()).unwrap_or_else(|| s.display());
    let mut details: Vec<(String, String)> = vec![];
    let mut link: Option<LinkInfo> = None;
    let mut card = None;
    let mut smart_info = None;
    if let Some(d) = drive {
        if !d.vendor.trim().is_empty() && !d.vendor.trim().starts_with("0x") {
            details.push(("Vendor".into(), d.vendor.trim().into()));
        }
        if !d.model.trim().is_empty() {
            details.push(("Model".into(), d.model.trim().into()));
        }
        if let Some(sn) = &d.serial {
            details.push(("Serial".into(), sn.clone()));
        }
        details.push(("Connection".into(), d.bus.label().to_string()));
        details.push(("Sector size".into(), format!("{} bytes", d.sector_size)));
        details.push(("Partitions".into(), d.partitions.len().to_string()));
    }
    #[cfg(target_os = "linux")]
    if let Some(dev) = &s.device {
        if let Some(n) = linux::block_name(dev) {
            if drive.map(|d| d.bus == crate::device::Bus::Usb).unwrap_or(false) || std::path::Path::new(&format!("/sys/block/{n}/device/../idVendor")).exists() || linux::usb_link(&n).is_some() {
                link = linux::usb_link(&n);
                details.extend(linux::usb_strings(&n));
            }
            card = linux::card(&n);
            // USB bridges and virtual disks routinely claim "rotational" without knowing, so only trust the flag where it is meaningful.
            let removable_or_virtual = drive.map(|d| matches!(d.bus, crate::device::Bus::Usb | crate::device::Bus::Virtual)).unwrap_or(false) || link.is_some();
            let is_virtual = drive.map(|d| d.bus == crate::device::Bus::Virtual).unwrap_or(false);
            match linux::rotational(&n) {
                // a loop device is a file, not "flash"
                Some(false) if !is_virtual => details.push(("Type".into(), "Flash".into())),
                Some(true) if !removable_or_virtual => details.push(("Type".into(), "Spinning disk".into())),
                _ => {}
            }
        }
    }
    if let Some(l) = &link {
        details.push(("USB link".into(), l.label.clone()));
        if let Some(p) = &l.protocol {
            details.push(("Transfer protocol".into(), p.clone()));
        }
    }
    if let Some(c) = &card {
        details.push(("Card".into(), format!("{} {} {}", c.kind, c.manufacturer, c.product).trim().to_string()));
        if !c.date.is_empty() {
            details.push(("Made".into(), c.date.clone()));
        }
    }
    if let Some(dev) = &s.device {
        smart_info = Some(smart(dev));
    }
    DeviceInfo {
        name,
        size: s.size,
        bus: drive.map(|d| format!("{:?}", d.bus).to_ascii_lowercase()).unwrap_or_else(|| "unknown".into()),
        removable: drive.map(|d| d.removable).unwrap_or(false),
        details,
        link,
        smart: smart_info,
        card,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usb_speeds_have_names_and_ceilings() {
        assert_eq!(usb_label(480.0).1, Some(42.0));
        assert!(usb_label(5000.0).0.contains("3.0"));
        assert!(usb_label(10000.0).0.contains("10 Gb/s"));
        assert!(usb_label(12.0).0.contains("1.1"));
    }

    #[test]
    fn card_makers() {
        assert_eq!(sd_manufacturer(0x03), Some("SanDisk"));
        assert_eq!(sd_manufacturer(0x1b), Some("Samsung"));
        assert_eq!(sd_manufacturer(0xEE), None);
    }
}
