//! Who is running the window, and how privileged work gets its rights.
//!
//! * Windows: the window starts as a normal user. Writing to a drive needs an administrator, so while
//!   it is not one the window says so and offers "Restart as administrator" (the normal UAC prompt).
//! * Linux: the window stays unprivileged; each drive operation is started through `pkexec`, which shows the
//!   system's own password prompt. Nothing here can approve that prompt on its own.
//! * macOS: same idea with the system's "do shell script … with administrator privileges" prompt.
//!
//! The one exception is a window that was started as root (sudo) or as an administrator: then
//! the helper simply inherits that.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Privilege {
    /// Running as root / an elevated administrator: helpers inherit it.
    Elevated,
    /// A system prompt appears each time a drive operation starts (Linux, macOS).
    AskPerTask,
    /// Windows without administrator rights: drive operations cannot start until the app is restarted elevated.
    NeedsRestart,
}

pub fn privilege() -> Privilege {
    // Design aid: NYFORGE_GUI_PRIVILEGE_PREVIEW=restart|ask shows how the window looks for a standard user on
    // Windows / Linux. It only changes what the window displays; the engine still needs real rights to write.
    match std::env::var("NYFORGE_GUI_PRIVILEGE_PREVIEW").as_deref() {
        Ok("restart") => return Privilege::NeedsRestart,
        Ok("ask") => return Privilege::AskPerTask,
        _ => {}
    }
    if nyforge_core::sys::is_root() {
        Privilege::Elevated
    } else if cfg!(windows) {
        Privilege::NeedsRestart
    } else {
        Privilege::AskPerTask
    }
}

/// Linux without polkit's `pkexec` cannot ask for a password graphically.
pub fn missing_prompt_tool() -> Option<&'static str> {
    if cfg!(target_os = "linux") && privilege() == Privilege::AskPerTask {
        let found = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).any(|d| d.join("pkexec").is_file())).unwrap_or(false);
        if !found {
            return Some("pkexec (polkit)");
        }
    }
    None
}

/// Windows: start this program again through the UAC prompt. `Ok(())` means Windows accepted the request and the
/// caller should close this window; the person may still decline the prompt, in which case nothing was started.
#[cfg(windows)]
pub fn restart_as_admin() -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let exe = std::env::current_exe().map_err(|e| format!("Could not find the program to restart: {e}"))?;
    let wide = |s: &std::ffi::OsStr| -> Vec<u16> { s.encode_wide().chain(std::iter::once(0)).collect() };
    let verb = wide(std::ffi::OsStr::new("runas"));
    let file = wide(exe.as_os_str());
    let r = unsafe { ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), file.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL) };
    if (r as usize) > 32 {
        Ok(())
    } else {
        Err("Windows did not start NyForge as administrator. The prompt may have been declined.".into())
    }
}

#[cfg(not(windows))]
pub fn restart_as_admin() -> Result<(), String> {
    Err("Restarting as administrator is only used on Windows.".into())
}
