//! The desktop notification when a long job ends while NyForge is in the background. It is a Windows toast
//! (bottom right of the screen, in the notification centre afterwards); other systems do nothing here.

use crate::run::Outcome;
use eframe::egui;

/// Shows "title / body" as a notification, but only when the window is not the one in front.
pub fn finished(ctx: &egui::Context, title: &str, body: &str) {
    let focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
    if focused {
        return;
    }
    toast(title, body);
}

/// The notification for a finished job: the good news, or what went wrong. A job the person stopped says nothing.
pub fn job_done(ctx: &egui::Context, tool: &str, ok_title: &str, ok_body: &str, outcome: &Outcome) {
    match outcome {
        Outcome::Success => finished(ctx, ok_title, ok_body),
        Outcome::Failed(f) => finished(ctx, &format!("{tool} did not finish"), &f.title),
        _ => {}
    }
}

#[cfg(windows)]
fn toast(title: &str, body: &str) {
    use std::os::windows::process::CommandExt;
    // The text goes through environment variables, so nothing in a drive's name can break the script.
    const SCRIPT: &str = r#"
$ErrorActionPreference = 'SilentlyContinue'
[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] | Out-Null
[Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom.XmlDocument, ContentType = WindowsRuntime] | Out-Null
$t = [System.Security.SecurityElement]::Escape($env:NYFORGE_TOAST_TITLE)
$b = [System.Security.SecurityElement]::Escape($env:NYFORGE_TOAST_BODY)
$x = New-Object Windows.Data.Xml.Dom.XmlDocument
$x.LoadXml("<toast><visual><binding template='ToastGeneric'><text>$t</text><text>$b</text></binding></visual></toast>")
$n = [Windows.UI.Notifications.ToastNotification]::new($x)
[Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe').Show($n)
"#;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let (title, body) = (title.to_string(), body.to_string());
    std::thread::spawn(move || {
        let _ = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", SCRIPT])
            .env("NYFORGE_TOAST_TITLE", title)
            .env("NYFORGE_TOAST_BODY", body)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    });
}

#[cfg(not(windows))]
fn toast(_title: &str, _body: &str) {}
