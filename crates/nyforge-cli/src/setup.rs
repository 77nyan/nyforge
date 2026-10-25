//! First-boot setup options (`nyforge setup`, and `--hostname` etc. on `nyforge flash`).

use crate::{is_root, Res};
use clap::Args;
use nyforge_core::device::{self, TargetPolicy};
use nyforge_core::error::{ErrorKind, NyError};
use nyforge_core::firstboot::{self, FirstBoot, SetupReport, Style, Wifi};
use nyforge_core::writer::Target;
use std::io::BufRead;

#[derive(Args, Default, Clone)]
pub struct SetupArgs {
    /// Host name for the new system, e.g. mypi
    #[arg(long, value_name = "NAME")]
    pub hostname: Option<String>,
    /// User to create
    #[arg(long, value_name = "NAME")]
    pub user: Option<String>,
    /// Password for that user (visible in the process list: prefer --password-stdin)
    #[arg(long, value_name = "TEXT")]
    pub password: Option<String>,
    /// Read the user's password from the first line of standard input
    #[arg(long)]
    pub password_stdin: bool,
    /// Turn on SSH
    #[arg(long)]
    pub ssh: bool,
    /// Public SSH key: a .pub file or the key text (can be given several times)
    #[arg(long, value_name = "FILE|KEY")]
    pub ssh_key: Vec<String>,
    /// Wi-Fi network name
    #[arg(long, value_name = "NAME")]
    pub wifi_ssid: Option<String>,
    /// Wi-Fi password (leave out for an open network)
    #[arg(long, value_name = "TEXT")]
    pub wifi_password: Option<String>,
    /// Wi-Fi country code, e.g. DE
    #[arg(long, value_name = "CC")]
    pub wifi_country: Option<String>,
    /// The Wi-Fi network does not announce its name
    #[arg(long)]
    pub wifi_hidden: bool,
    /// Time zone, e.g. Europe/Berlin
    #[arg(long, value_name = "ZONE")]
    pub timezone: Option<String>,
}

impl SetupArgs {
    pub fn requested(&self) -> bool {
        self.hostname.is_some()
            || self.user.is_some()
            || self.password.is_some()
            || self.password_stdin
            || self.ssh
            || !self.ssh_key.is_empty()
            || self.wifi_ssid.is_some()
            || self.timezone.is_some()
    }

    pub fn to_firstboot(&self) -> Result<FirstBoot, NyError> {
        let mut password = self.password.clone();
        if self.password_stdin {
            let mut line = String::new();
            std::io::stdin().lock().read_line(&mut line).map_err(|e| NyError::from_io(&e, "reading the password"))?;
            password = Some(line.trim_end_matches(['\n', '\r']).to_string());
        }
        let mut keys = vec![];
        for k in &self.ssh_key {
            let p = std::path::Path::new(k);
            if p.is_file() {
                let text = std::fs::read_to_string(p).map_err(|e| NyError::from_io(&e, &format!("reading {k}")))?;
                if text.contains("PRIVATE KEY") {
                    return Err(NyError::invalid(format!("{k} is a private key.")).with_hint("Give the public half (the .pub file)."));
                }
                keys.extend(text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(String::from));
            } else {
                keys.push(k.trim().to_string());
            }
        }
        let wifi = match &self.wifi_ssid {
            Some(ssid) => Some(Wifi { ssid: ssid.clone(), password: self.wifi_password.clone(), country: self.wifi_country.clone().map(|c| c.to_ascii_uppercase()), hidden: self.wifi_hidden }),
            None if self.wifi_password.is_some() || self.wifi_country.is_some() || self.wifi_hidden => {
                return Err(NyError::invalid("Wi-Fi options need --wifi-ssid."));
            }
            None => None,
        };
        let fb = FirstBoot { hostname: self.hostname.clone(), username: self.user.clone(), password, ssh: self.ssh, ssh_keys: keys, wifi, timezone: self.timezone.clone() };
        firstboot::validate(&fb)?;
        Ok(fb)
    }
}

pub fn print_report(r: &SetupReport, target: &str) {
    let how = match r.style {
        Style::CloudInit => "cloud-init",
        Style::Legacy => "legacy marker files",
    };
    eprintln!("✔ First-boot setup written to {target} ({how}): {}", r.files.join(", "));
    for n in &r.notes {
        eprintln!("  - {n}");
    }
}

#[derive(Args)]
pub struct SetupCmd {
    /// Drive (or image file) that already holds a Raspberry Pi style image
    pub target: String,
    #[command(flatten)]
    pub setup: SetupArgs,
    #[arg(long)]
    pub allow_internal: bool,
    #[arg(long)]
    pub allow_virtual: bool,
    #[arg(long)]
    pub json: bool,
}

pub fn run(a: SetupCmd) -> Res {
    let fb = a.setup.to_firstboot()?;
    let policy = TargetPolicy { allow_internal: a.allow_internal, allow_virtual: a.allow_virtual };
    let t = Target::from_resolved(device::resolve_target(&a.target, policy)?);
    if t.drive.is_some() && !is_root() {
        return Err(NyError::new(ErrorKind::Permission, "Changing a drive needs administrator rights.").with_hint(nyforge_core::sys::elevation_hint()));
    }
    let rep = firstboot::apply(&t, &fb, None)?;
    if a.json {
        println!("{}", serde_json::to_string(&rep).unwrap());
    } else {
        print_report(&rep, &t.display);
    }
    Ok(0)
}
