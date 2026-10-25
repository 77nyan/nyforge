//! "Reduce motion": honour the operating system's setting where it can be read, or NYFORGE_REDUCE_MOTION=1.

pub fn system_prefers_reduced_motion() -> bool {
    if let Ok(v) = std::env::var("NYFORGE_REDUCE_MOTION") {
        return !matches!(v.trim(), "" | "0" | "false" | "no");
    }
    platform()
}

#[cfg(target_os = "linux")]
fn platform() -> bool {
    // GNOME (and most desktops built on it) keep this switch; anything unreadable counts as "animations on".
    std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "enable-animations"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "false")
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
fn platform() -> bool {
    std::process::Command::new("defaults")
        .args(["read", "com.apple.universalaccess", "reduceMotion"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "1")
        .unwrap_or(false)
}

#[cfg(windows)]
fn platform() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SystemParametersInfoW, SPI_GETCLIENTAREAANIMATION};
    let mut on: i32 = 1;
    let ok = unsafe { SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, &mut on as *mut i32 as *mut _, 0) };
    ok != 0 && on == 0
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn platform() -> bool {
    false
}
