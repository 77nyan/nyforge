//! `nyforge format` from the outside: the safety promise and the output formats.

use std::path::PathBuf;
use std::process::{Command, Output};

fn nyforge(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_nyforge")).args(args).output().expect("run nyforge")
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nyforge-cli-{}-{}", std::process::id(), name));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn there_is_no_flag_that_unlocks_pc_drives() {
    for flag in ["--allow-internal", "--allow-virtual", "--force", "--allow-system"] {
        let o = nyforge(&["format", "/dev/null", "-y", flag]);
        assert_eq!(o.status.code(), Some(2), "{flag} must be rejected by the argument parser: {}", String::from_utf8_lossy(&o.stderr));
        assert!(String::from_utf8_lossy(&o.stderr).contains("unexpected argument"));
    }
    let help = String::from_utf8_lossy(&nyforge(&["format", "--help"]).stdout).to_string();
    assert!(!help.contains("allow-internal") && !help.contains("allow-virtual"), "{help}");
}

#[test]
fn the_old_name_works_as_an_alias() {
    let o = nyforge(&["formatusb", "--list", "--json"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).expect("--list --json is valid JSON");
    assert!(v.is_array());
    // whatever is listed is removable media only
    for d in v.as_array().unwrap() {
        assert_eq!(d["safety"], "safe", "{d}");
        assert!(d["bus"] == "usb" || d["bus"] == "sd", "{d}");
    }
}

#[test]
fn formats_an_image_file_and_reports_json() {
    let d = tmp("json");
    let img = d.join("s.img");
    let o = nyforge(&["format", img.to_str().unwrap(), "--size", "128M", "--fs", "exfat", "-l", "Stick", "--json"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let lines: Vec<serde_json::Value> = String::from_utf8_lossy(&o.stdout).lines().map(|l| serde_json::from_str(l).expect(l)).collect();
    assert_eq!(lines.first().unwrap()["event"], "start");
    let rep = lines.last().unwrap();
    assert_eq!(rep["event"], "format_report");
    assert_eq!(rep["partitions"][0]["fs"], "exfat");
    assert_eq!(rep["partitions"][0]["label"], "Stick");
    assert_eq!(rep["scheme"], "mbr");
    assert_eq!(std::fs::metadata(&img).unwrap().len(), 128 << 20);
    std::fs::remove_dir_all(d).ok();
}

#[test]
fn mistakes_are_explained_and_exit_nonzero_without_creating_anything() {
    let d = tmp("mistakes");
    let img = d.join("x.img");
    let p = img.to_str().unwrap();
    for (args, expect) in [
        (vec!["format", p, "--size", "64M", "--fs", "btrfs"], "not a filesystem"),
        (vec!["format", p, "--size", "64M", "--label", "a*b"], "cannot be used"),
        (vec!["format", p, "--size", "64M", "--wipe", "shred"], "not a wipe mode"),
        (vec!["format", p, "--size", "64M", "--fs", "fat32", "--partition", "exfat"], "describes everything itself"),
        (vec!["format", p, "--size", "64M", "--test-capacity", "sometimes"], "capacity test mode"),
        (vec!["format", p, "--size", "64M", "--partition", "fat32:1G", "--partition", "exfat"], "needs"),
        (vec!["format", p, "--size", "64M", "--fit-real-capacity"], "--test-capacity full"),
        (vec!["format", p], "needs a size"),
        (vec!["format", p, "--size", "64M", "--cluster-size", "3000"], "power of two"),
    ] {
        let o = nyforge(&args);
        let err = String::from_utf8_lossy(&o.stderr).to_string();
        assert_eq!(o.status.code(), Some(1), "{args:?}: {err}");
        assert!(err.contains(expect), "{args:?} should say \"{expect}\": {err}");
        assert!(!img.exists(), "{args:?} created the image although the request was invalid");
    }
    std::fs::remove_dir_all(d).ok();
}

#[test]
fn json_mode_refuses_to_guess_the_drive() {
    let o = nyforge(&["format", "--json"]);
    assert_eq!(o.status.code(), Some(1));
}
