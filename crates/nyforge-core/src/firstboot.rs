//! First-boot setup for Raspberry-Pi-style images: hostname, user, password,
//! SSH, Wi-Fi, time zone. Written straight into the image's FAT boot partition
//! (no mounting), after or instead of flashing.
//!
//! Two mechanisms, chosen by looking at what the image ships with:
//! * **cloud-init** (Raspberry Pi OS "Trixie" and newer, Ubuntu for Pi): the
//!   boot partition has a `user-data` file; we write `user-data` + `network-config`.
//! * **legacy** (older Raspberry Pi OS): marker files `ssh`, `userconf.txt` and,
//!   for pre-Bookworm images, `wpa_supplicant.conf`.
//!
//! Honest status: the file contents follow Raspberry Pi's published formats and
//! are verified here by reading them back from a FAT image; they were not tried
//! on a real Pi.

use crate::error::{ErrorKind, NyError, Result};
use crate::nyboot::io::{FileIo, Io};
use crate::nyboot::stream::CachedStream;
use crate::partition::{self, Partition, SECTOR};
use crate::sys::{self, TargetKind};
use crate::writer::Target;
use fatfs::{FileSystem, FsOptions};
use std::io::{Read, Seek, SeekFrom, Write};

#[derive(Debug, Clone, Default)]
pub struct Wifi {
    pub ssid: String,
    pub password: Option<String>,
    /// Two-letter regulatory country, e.g. "DE".
    pub country: Option<String>,
    pub hidden: bool,
}

#[derive(Debug, Clone, Default)]
pub struct FirstBoot {
    pub hostname: Option<String>,
    pub username: Option<String>,
    /// Plain text; only a SHA-512 hash is ever written to the drive.
    pub password: Option<String>,
    pub ssh: bool,
    pub ssh_keys: Vec<String>,
    pub wifi: Option<Wifi>,
    pub timezone: Option<String>,
}

impl FirstBoot {
    pub fn is_empty(&self) -> bool {
        self.hostname.is_none() && self.username.is_none() && self.password.is_none() && !self.ssh && self.ssh_keys.is_empty() && self.wifi.is_none() && self.timezone.is_none()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Style {
    CloudInit,
    Legacy,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SetupReport {
    pub style: Style,
    pub partition: u32,
    pub files: Vec<String>,
    pub notes: Vec<String>,
}

fn invalid(m: impl Into<String>) -> NyError {
    NyError::new(ErrorKind::InvalidInput, m)
}

pub fn validate(fb: &FirstBoot) -> Result<()> {
    if let Some(h) = &fb.hostname {
        let ok = !h.is_empty() && h.len() <= 63 && !h.starts_with('-') && !h.ends_with('-') && h.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        if !ok {
            return Err(invalid(format!("\"{h}\" is not a valid host name.")).with_hint("Use letters, digits and dashes, up to 63 characters (for example: mypi)."));
        }
    }
    if let Some(u) = &fb.username {
        let mut cs = u.chars();
        let first_ok = cs.next().map_or(false, |c| c.is_ascii_lowercase() || c == '_');
        if !first_ok || u.len() > 32 || !u.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-') {
            return Err(invalid(format!("\"{u}\" is not a valid user name.")).with_hint("Lowercase letters, digits, - and _; it must not start with a digit."));
        }
        if u == "root" {
            return Err(invalid("The user name cannot be \"root\"."));
        }
    }
    if fb.password.is_some() && fb.username.is_none() {
        return Err(invalid("A password needs a user name.").with_hint("Add --user NAME."));
    }
    if let Some(p) = &fb.password {
        if p.is_empty() || p.contains(['\n', '\r']) {
            return Err(invalid("The password is empty or contains a line break."));
        }
    }
    for k in &fb.ssh_keys {
        let k = k.trim();
        let ok = ["ssh-ed25519 ", "ssh-rsa ", "ecdsa-sha2-", "sk-ssh-ed25519@", "sk-ecdsa-sha2-"].iter().any(|p| k.starts_with(p)) && !k.contains(['\n', '\r', '"']);
        if !ok {
            return Err(invalid("That does not look like a public SSH key.").with_hint("Give the contents of your .pub file (never the private key)."));
        }
    }
    if !fb.ssh_keys.is_empty() && fb.username.is_none() {
        return Err(invalid("SSH keys need a user name.").with_hint("Add --user NAME."));
    }
    if let Some(w) = &fb.wifi {
        if w.ssid.is_empty() || w.ssid.len() > 32 || w.ssid.contains(['\n', '\r', '"', '\\']) {
            return Err(invalid("The Wi-Fi name must be 1–32 characters without quotes or backslashes."));
        }
        if let Some(p) = &w.password {
            if !(8..=63).contains(&p.len()) || p.contains(['\n', '\r', '"', '\\']) {
                return Err(invalid("A Wi-Fi password must be 8–63 characters without quotes or backslashes.").with_hint("Leave it out for an open network."));
            }
        }
        if let Some(c) = &w.country {
            if c.len() != 2 || !c.chars().all(|c| c.is_ascii_uppercase()) {
                return Err(invalid("The Wi-Fi country must be a two-letter code such as DE, GB or US."));
            }
        }
    }
    if let Some(t) = &fb.timezone {
        let ok = t.contains('/') && t.chars().all(|c| c.is_ascii_alphanumeric() || "/_-+".contains(c)) && !t.contains("..");
        if !ok {
            return Err(invalid(format!("\"{t}\" is not a time zone like Europe/Berlin.")));
        }
    }
    Ok(())
}

pub fn hash_password(pw: &str) -> Result<String> {
    let params = sha_crypt::Sha512Params::new(sha_crypt::ROUNDS_DEFAULT).map_err(|_| NyError::other("password hashing failed"))?;
    sha_crypt::sha512_simple(pw, &params).map_err(|_| NyError::other("password hashing failed"))
}

fn q(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// cloud-init `user-data`. `pi_groups`: the image is a Raspberry Pi OS (its hardware groups exist).
pub fn cloud_user_data(fb: &FirstBoot, password_hash: Option<&str>, pi_groups: bool) -> String {
    let mut s = String::from("#cloud-config\n# Written by NyForge\n");
    if let Some(h) = &fb.hostname {
        s += &format!("hostname: {h}\nmanage_etc_hosts: true\n");
    }
    if let Some(t) = &fb.timezone {
        s += &format!("timezone: {t}\n");
    }
    if let Some(u) = &fb.username {
        let groups = if pi_groups { "users,adm,dialout,audio,netdev,video,plugdev,cdrom,games,input,gpio,spi,i2c,render,sudo" } else { "users,adm,sudo" };
        s += &format!("users:\n  - name: {u}\n    groups: {groups}\n    shell: /bin/bash\n    sudo: ALL=(ALL) NOPASSWD:ALL\n");
        match password_hash {
            Some(h) => s += &format!("    lock_passwd: false\n    passwd: {}\n", q(h)),
            None => s += "    lock_passwd: true\n",
        }
        if !fb.ssh_keys.is_empty() {
            s += "    ssh_authorized_keys:\n";
            for k in &fb.ssh_keys {
                s += &format!("      - {}\n", q(k.trim()));
            }
        }
    }
    if fb.ssh || !fb.ssh_keys.is_empty() {
        s += "enable_ssh: true\n";
        if password_hash.is_some() {
            s += "ssh_pwauth: true\n";
        }
    }
    s
}

/// Netplan `network-config` for Wi-Fi.
pub fn cloud_network_config(w: &Wifi) -> String {
    let mut s = String::from("network:\n  version: 2\n  wifis:\n    renderer: NetworkManager\n    wlan0:\n      dhcp4: true\n");
    if let Some(c) = &w.country {
        s += &format!("      regulatory-domain: {}\n", q(c));
    }
    s += "      access-points:\n";
    s += &format!("        {}:\n", q(&w.ssid));
    if let Some(p) = &w.password {
        s += &format!("          password: {}\n", q(p));
    }
    if w.hidden {
        s += "          hidden: true\n";
    }
    s += "      optional: true\n";
    s
}

/// `wpa_supplicant.conf` for older images.
pub fn wpa_supplicant_conf(w: &Wifi) -> String {
    let mut s = String::from("ctrl_interface=DIR=/var/run/wpa_supplicant GROUP=netdev\nupdate_config=1\n");
    if let Some(c) = &w.country {
        s += &format!("country={c}\n");
    }
    s += "network={\n";
    s += &format!("    ssid=\"{}\"\n", w.ssid);
    match &w.password {
        Some(p) => s += &format!("    psk=\"{p}\"\n"),
        None => s += "    key_mgmt=NONE\n",
    }
    if w.hidden {
        s += "    scan_ssid=1\n";
    }
    s += "}\n";
    s
}

fn fat_err(e: std::io::Error) -> NyError {
    NyError::from_io(&e, "writing the setup files")
}

fn find_boot_partition(whole: &FileIo) -> Result<Partition> {
    let table = partition::read_table(whole)?;
    for p in &table {
        if p.start * SECTOR + p.bytes() <= whole.len() && partition::looks_like_fat(whole, p) {
            return Ok(p.clone());
        }
    }
    Err(NyError::new(ErrorKind::ImageUnsupported, "No FAT boot partition found on this drive.")
        .with_hint("First-boot setup works on Raspberry Pi style images, where the first partition is a FAT partition with config.txt."))
}

/// Apply `fb` to the image on `target` (already written).
pub fn apply(target: &Target, fb: &FirstBoot, force_style: Option<Style>) -> Result<SetupReport> {
    validate(fb)?;
    if fb.is_empty() {
        return Err(invalid("Nothing to set up.").with_hint("Give at least one of --hostname, --user, --ssh, --wifi-ssid, --timezone."));
    }
    let _prep = match &target.drive {
        Some(d) => Some(sys::prepare_device(d, true)?),
        None => None,
    };
    let file = match target.kind {
        TargetKind::BlockDevice => sys::open_rw(&target.path, true),
        TargetKind::File => std::fs::OpenOptions::new().read(true).write(true).open(&target.path),
    }
    .map_err(|e| NyError::from_io(&e, &format!("opening {}", target.display)))?;
    let len = match target.kind {
        TargetKind::BlockDevice => target.capacity.ok_or_else(|| NyError::other("unknown drive size"))?,
        TargetKind::File => file.metadata().map_err(|e| NyError::from_io(&e, "reading the image size"))?.len(),
    };
    let align = sys::block_io_align(target.kind);
    let whole = FileIo::new(file, 0, len / SECTOR * SECTOR, align);
    let part = find_boot_partition(&whole)?;
    let pio = whole.sub(part.start * SECTOR, part.bytes()).map_err(|e| NyError::from_io(&e, "opening the boot partition"))?;
    let mut cs = CachedStream::new(pio);
    cs.seek(SeekFrom::Start(0)).map_err(fat_err)?;

    let mut files = vec![];
    let mut notes = vec![];
    let style;
    {
        let fs = FileSystem::new(&mut cs, FsOptions::new()).map_err(fat_err)?;
        let label = fs.volume_label().trim().to_ascii_lowercase();
        {
            let root = fs.root_dir();
            let mut names: Vec<String> = vec![];
            for e in root.iter() {
                names.push(e.map_err(fat_err)?.file_name().to_ascii_lowercase());
            }
            let has = |n: &str| names.iter().any(|x| x == n);
            style = match force_style {
                Some(s) => s,
                None if has("user-data") => Style::CloudInit,
                None if has("config.txt") || has("cmdline.txt") => Style::Legacy,
                None => {
                    return Err(NyError::new(ErrorKind::ImageUnsupported, "This does not look like a Raspberry Pi boot partition (no config.txt or user-data).")
                        .with_hint("First-boot setup is for Raspberry Pi style images."))
                }
            };
            let mut put = |name: &str, data: &[u8]| -> Result<()> {
                // Replace an existing file of that name (case-insensitively).
                if let Some(old) = names.iter().find(|n| n.eq_ignore_ascii_case(name)) {
                    let _ = root.remove(old);
                    let _ = root.remove(name);
                }
                let mut f = root.create_file(name).map_err(fat_err)?;
                f.truncate().map_err(fat_err)?;
                f.write_all(data).map_err(fat_err)?;
                f.flush().map_err(fat_err)?;
                files.push(name.to_string());
                Ok(())
            };
            let hash = match &fb.password {
                Some(p) => Some(hash_password(p)?),
                None => None,
            };
            match style {
                Style::CloudInit => {
                    let pi = label == "bootfs" || label == "boot" || has("config.txt") && !label.contains("system-boot");
                    put("user-data", cloud_user_data(fb, hash.as_deref(), pi).as_bytes())?;
                    if let Some(w) = &fb.wifi {
                        put("network-config", cloud_network_config(w).as_bytes())?;
                    }
                    if !has("meta-data") {
                        put("meta-data", b"instance_id: nyforge\n")?;
                    }
                    if fb.password.is_some() {
                        notes.push("Only a salted hash of the password was written to the drive.".into());
                    }
                }
                Style::Legacy => {
                    if fb.ssh || !fb.ssh_keys.is_empty() {
                        put("ssh", b"")?;
                    }
                    if let (Some(u), Some(h)) = (&fb.username, &hash) {
                        put("userconf.txt", format!("{u}:{h}\n").as_bytes())?;
                    } else if fb.username.is_some() {
                        notes.push("A user name without a password was ignored: older images need both.".into());
                    }
                    if let Some(w) = &fb.wifi {
                        put("wpa_supplicant.conf", wpa_supplicant_conf(w).as_bytes())?;
                        notes.push("Wi-Fi was written as wpa_supplicant.conf, which Raspberry Pi OS Bookworm ignores (it uses NetworkManager). Use a Trixie or newer image, or set Wi-Fi after boot.".into());
                    }
                    if !fb.ssh_keys.is_empty() {
                        notes.push("SSH keys are only supported on images with cloud-init; SSH itself was enabled.".into());
                    }
                    if fb.hostname.is_some() || fb.timezone.is_some() {
                        notes.push("Host name and time zone are only supported on images with cloud-init (Raspberry Pi OS Trixie or newer, Ubuntu for Pi); they were skipped.".into());
                    }
                }
            }
        }
        let _ = fs.stats();
        fs.unmount().map_err(fat_err)?;
    }
    cs.flush_all().map_err(fat_err)?;
    whole.sync().map_err(|e| NyError::from_io(&e, "flushing the drive"))?;
    drop(cs);
    drop(whole);
    if target.kind == TargetKind::BlockDevice {
        sys::finish_device(&target.path);
    }
    // Unused-import guard for platforms where Read is not needed.
    let _ = std::io::empty().read(&mut []);
    Ok(SetupReport { style, partition: part.index, files, notes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn have(c: &str) -> bool {
        Command::new("sh").args(["-c", &format!("command -v {c} >/dev/null")]).status().map(|s| s.success()).unwrap_or(false)
    }

    fn full() -> FirstBoot {
        FirstBoot {
            hostname: Some("mypi".into()),
            username: Some("alex".into()),
            password: Some("s3cret pass".into()),
            ssh: true,
            ssh_keys: vec!["ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExample alex@laptop".into()],
            wifi: Some(Wifi { ssid: "Home Net".into(), password: Some("hunter2hunter2".into()), country: Some("DE".into()), hidden: true }),
            timezone: Some("Europe/Berlin".into()),
        }
    }

    #[test]
    fn validation_catches_bad_input() {
        assert!(validate(&full()).is_ok());
        let bad = |f: fn(&mut FirstBoot)| {
            let mut x = full();
            f(&mut x);
            validate(&x).is_err()
        };
        assert!(bad(|x| x.hostname = Some("-bad".into())));
        assert!(bad(|x| x.hostname = Some("has space".into())));
        assert!(bad(|x| x.username = Some("Root User".into())));
        assert!(bad(|x| x.username = Some("root".into())));
        assert!(bad(|x| x.ssh_keys = vec!["PRIVATE KEY".into()]));
        assert!(bad(|x| x.wifi.as_mut().unwrap().password = Some("short".into())));
        assert!(bad(|x| x.wifi.as_mut().unwrap().ssid = "quote\"d".into()));
        assert!(bad(|x| x.wifi.as_mut().unwrap().country = Some("de".into())));
        assert!(bad(|x| x.timezone = Some("Berlin".into())));
        assert!(bad(|x| {
            x.username = None;
        }));
    }

    #[test]
    fn password_hash_is_a_valid_sha512_crypt() {
        let h = hash_password("s3cret pass").unwrap();
        assert!(h.starts_with("$6$"));
        assert!(sha_crypt::sha512_check("s3cret pass", &h).is_ok());
        assert!(sha_crypt::sha512_check("wrong", &h).is_err());
        if have("openssl") {
            // Independent implementation computes the same hash from the same salt.
            let salt = h.split('$').nth(2).unwrap();
            let o = Command::new("openssl").args(["passwd", "-6", "-salt", salt, "s3cret pass"]).output().unwrap();
            assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), h);
        }
    }

    #[test]
    fn cloud_init_files_have_the_documented_shape() {
        let f = full();
        let ud = cloud_user_data(&f, Some("$6$salt$hash"), true);
        assert!(ud.starts_with("#cloud-config\n"));
        for want in ["hostname: mypi", "timezone: Europe/Berlin", "- name: alex", "lock_passwd: false", "passwd: \"$6$salt$hash\"", "ssh_authorized_keys:", "enable_ssh: true", "ssh_pwauth: true", "gpio"] {
            assert!(ud.contains(want), "{want}\n{ud}");
        }
        assert!(!ud.contains("s3cret"), "plain password must not appear");
        let nc = cloud_network_config(f.wifi.as_ref().unwrap());
        for want in ["version: 2", "renderer: NetworkManager", "regulatory-domain: \"DE\"", "\"Home Net\":", "password: \"hunter2hunter2\"", "hidden: true"] {
            assert!(nc.contains(want), "{want}\n{nc}");
        }
        let ud2 = cloud_user_data(&f, Some("h"), false);
        assert!(!ud2.contains("gpio") && ud2.contains("groups: users,adm,sudo"));
        // The text parses as YAML.
        if have("python3") {
            for (name, text) in [("user-data", &ud), ("network-config", &nc)] {
                let o = Command::new("python3").args(["-c", "import sys,yaml;yaml.safe_load(sys.stdin.read())"]).stdin(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).stdout(std::process::Stdio::null()).spawn();
                if let Ok(mut c) = o {
                    c.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
                    let out = c.wait_with_output().unwrap();
                    let err = String::from_utf8_lossy(&out.stderr);
                    if !err.contains("No module named") {
                        assert!(out.status.success(), "{name} is not valid YAML: {err}");
                    }
                }
            }
        }
    }

    #[test]
    fn legacy_wpa_file_is_well_formed() {
        let w = full().wifi.unwrap();
        let t = wpa_supplicant_conf(&w);
        assert!(t.contains("country=DE") && t.contains("ssid=\"Home Net\"") && t.contains("psk=\"hunter2hunter2\"") && t.contains("scan_ssid=1"));
        let open = wpa_supplicant_conf(&Wifi { ssid: "Cafe".into(), ..Default::default() });
        assert!(open.contains("key_mgmt=NONE"));
    }

    /// These tests need external tools that only Linux machines have. On Linux a missing tool is an error (CI installs
    /// them); on macOS and Windows the test is skipped, because those systems cannot run it at all.
    fn skip_without(tools: &[&str]) -> bool {
        let missing: Vec<&&str> = tools.iter().filter(|t| !have(t)).collect();
        if missing.is_empty() {
            return false;
        }
        assert!(!cfg!(target_os = "linux"), "{missing:?} required for this test");
        eprintln!("skipped: {missing:?} not available on this system");
        true
    }

    /// A Pi-like image: partition table by `parted`, FAT32 boot partition by mtools, plus a dummy root partition.
    fn pi_image(name: &str, label: &str, files: &[&str], gpt: bool) -> std::path::PathBuf {
        for t in ["parted", "mformat", "mcopy", "mtype"] {
            assert!(have(t), "{t} is required for this test");
        }
        let p = std::env::temp_dir().join(format!("nyforge-pi-{}-{}.img", std::process::id(), name));
        let f = std::fs::File::create(&p).unwrap();
        f.set_len(128 << 20).unwrap();
        drop(f);
        let o = Command::new("parted")
            .arg("-s")
            .arg(&p)
            .args(["mklabel", if gpt { "gpt" } else { "msdos" }, "mkpart", if gpt { "boot" } else { "primary" }, "fat32", "8192s", "139263s", "mkpart", if gpt { "root" } else { "primary" }, "ext4", "139264s", "204799s"])
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let spec = format!("{}@@{}", p.display(), 8192 * 512);
        assert!(Command::new("mformat").args(["-i", &spec, "-F", "-v", label, "-T", "131072", "::"]).status().unwrap().success());
        for f in files {
            let tmp = std::env::temp_dir().join(format!("nyforge-pi-src-{}-{}", std::process::id(), f));
            std::fs::write(&tmp, format!("original {f}\n")).unwrap();
            assert!(Command::new("mcopy").args(["-i", &spec]).arg(&tmp).arg(format!("::{f}")).status().unwrap().success());
        }
        p
    }

    fn read_back(img: &std::path::Path, name: &str) -> Option<String> {
        let spec = format!("{}@@{}", img.display(), 8192 * 512);
        let o = Command::new("mtype").args(["-i", &spec, &format!("::{name}")]).output().ok()?;
        o.status.success().then(|| String::from_utf8_lossy(&o.stdout).into_owned())
    }

    fn fsck_boot(img: &std::path::Path) {
        if !have("fsck.vfat") {
            return;
        }
        let part = img.with_extension("part");
        let st = Command::new("dd").arg(format!("if={}", img.display())).arg(format!("of={}", part.display())).args(["bs=512", "skip=8192", "count=131072", "status=none"]).status().unwrap();
        assert!(st.success());
        let o = Command::new("fsck.vfat").arg("-n").arg(&part).output().unwrap();
        assert!(o.status.success(), "fsck.vfat: {}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    }

    #[test]
    fn writes_cloud_init_into_a_trixie_style_image_mbr_and_gpt() {
        for (name, gpt) in [("ci-mbr", false), ("ci-gpt", true)] {
            if skip_without(&["parted", "mformat", "mcopy", "mtype"]) {
                return;
            }
            let img = pi_image(name, "BOOTFS", &["config.txt", "cmdline.txt", "user-data", "network-config", "meta-data"], gpt);
            let rep = apply(&Target::file(&img), &full(), None).unwrap();
            assert_eq!(rep.style, Style::CloudInit);
            assert_eq!(rep.partition, 1);
            let ud = read_back(&img, "user-data").unwrap();
            assert!(ud.contains("hostname: mypi") && ud.contains("passwd: \"$6$"), "{ud}");
            assert!(!ud.contains("original"), "old file must be replaced");
            let hash = ud.lines().find(|l| l.trim_start().starts_with("passwd:")).unwrap().split('"').nth(1).unwrap().to_string();
            assert!(sha_crypt::sha512_check("s3cret pass", &hash).is_ok());
            assert!(read_back(&img, "network-config").unwrap().contains("Home Net"));
            assert_eq!(read_back(&img, "config.txt").unwrap(), "original config.txt\n", "other files untouched");
            fsck_boot(&img);
        }
    }

    #[test]
    fn writes_legacy_marker_files_for_older_images() {
        if skip_without(&["parted", "mformat", "mcopy", "mtype"]) {
            return;
        }
        let img = pi_image("legacy", "BOOTFS", &["config.txt", "cmdline.txt"], false);
        let rep = apply(&Target::file(&img), &full(), None).unwrap();
        assert_eq!(rep.style, Style::Legacy);
        assert!(rep.files.contains(&"ssh".to_string()) && rep.files.contains(&"userconf.txt".to_string()) && rep.files.contains(&"wpa_supplicant.conf".to_string()));
        let uc = read_back(&img, "userconf.txt").unwrap();
        assert!(uc.starts_with("alex:$6$"));
        assert!(rep.notes.iter().any(|n| n.contains("Bookworm")));
        assert!(rep.notes.iter().any(|n| n.contains("time zone")));
        fsck_boot(&img);
    }

    #[test]
    fn refuses_drives_that_are_not_pi_images() {
        if skip_without(&["parted", "mformat", "mcopy", "mtype"]) {
            return;
        }
        let img = pi_image("other", "DATA", &["readme.txt"], false);
        let e = apply(&Target::file(&img), &full(), None).err().unwrap();
        assert_eq!(e.kind, ErrorKind::ImageUnsupported);
        let blank = std::env::temp_dir().join(format!("nyforge-pi-blank-{}.img", std::process::id()));
        std::fs::write(&blank, vec![0u8; 8 << 20]).unwrap();
        assert_eq!(apply(&Target::file(&blank), &full(), None).err().unwrap().kind, ErrorKind::ImageUnsupported);
        let e = apply(&Target::file(&blank), &FirstBoot::default(), None).err().unwrap();
        assert_eq!(e.kind, ErrorKind::InvalidInput);
    }
}
