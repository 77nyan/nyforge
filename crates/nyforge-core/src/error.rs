//! Errors that explain themselves.
//!
//! Every failure carries a machine-readable [`ErrorKind`], a plain-language
//! message and (where we know one) a concrete hint for fixing it.  Front ends
//! show `message` and `hint`; scripts branch on `kind`.

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// Not allowed to touch the device (needs administrator / root).
    Permission,
    /// Image is bigger than the drive, or the drive filled up.
    NoSpace,
    /// Something else is using the drive (mounted, open elsewhere).
    Busy,
    /// Drive vanished mid-write (unplugged, port reset).
    DeviceGone,
    /// The drive or its cable reported a hardware-level error.
    Hardware,
    /// Write-protected medium.
    ReadOnly,
    /// The image file is damaged or truncated.
    ImageCorrupt,
    /// We do not know how to read this kind of image.
    ImageUnsupported,
    /// Download problems.
    Network,
    /// The user cancelled.
    Cancelled,
    /// A safety rule refused the operation (system drive, etc.).
    Unsafe,
    /// Bad arguments or unusable input.
    InvalidInput,
    /// A required external tool or feature is missing on this platform.
    Unavailable,
    /// The drive holds less data than it claims (counterfeit or failing flash).
    FakeCapacity,
    /// Anything else.
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NyError {
    pub kind: ErrorKind,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

pub type Result<T> = std::result::Result<T, NyError>;

/// "The drive went away" errno values. `ENOMEDIUM` only exists on Linux.
#[cfg(unix)]
fn errno_is_gone(c: i32) -> bool {
    let gone = c == libc::ENODEV || c == libc::ENXIO || c == libc::ESHUTDOWN;
    #[cfg(target_os = "linux")]
    let gone = gone || c == libc::ENOMEDIUM;
    gone
}

/// "The hardware misbehaved" errno values. `EREMOTEIO` and `ECOMM` only exist on Linux.
#[cfg(unix)]
fn errno_is_hardware(c: i32) -> bool {
    let hw = c == libc::EIO || c == libc::ETIMEDOUT || c == libc::EILSEQ || c == libc::EPROTO;
    #[cfg(target_os = "linux")]
    let hw = hw || c == libc::EREMOTEIO || c == libc::ECOMM;
    hw
}

impl NyError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into(), hint: None }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn other(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Other, message)
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::InvalidInput, message)
    }

    pub fn cancelled() -> Self {
        Self::new(ErrorKind::Cancelled, "Cancelled.")
    }

    /// Translate an OS error into something a human can act on.
    /// `what` describes what we were doing, e.g. "writing to /dev/sdb".
    pub fn from_io(err: &std::io::Error, what: &str) -> Self {
        let raw = err.raw_os_error();
        #[cfg(unix)]
        {
            use libc::*;
            if let Some(code) = raw {
                let (kind, msg, hint): (ErrorKind, String, Option<&str>) = match code {
                    EACCES | EPERM => (
                        ErrorKind::Permission,
                        format!("Permission denied while {what}."),
                        Some("Writing to drives needs root. Re-run with sudo, or use the NyForge app, which asks for permission itself."),
                    ),
                    ENOSPC | EFBIG => (
                        ErrorKind::NoSpace,
                        format!("The drive ran out of space while {what}."),
                        Some("Use a larger drive, or a smaller image."),
                    ),
                    EBUSY => (
                        ErrorKind::Busy,
                        format!("The drive is busy while {what}."),
                        Some("Something is still using it. Close any windows or programs showing its files, unmount it, and try again."),
                    ),
                    c if errno_is_gone(c) => (
                        ErrorKind::DeviceGone,
                        format!("The drive disappeared while {what}."),
                        Some("It was unplugged or its port reset. Reconnect it (try a different port) and start again."),
                    ),
                    c if errno_is_hardware(c) => (
                        ErrorKind::Hardware,
                        format!("The drive reported a hardware error while {what}."),
                        Some("The drive, card reader or cable may be failing. Try another port or reader first, then another drive."),
                    ),
                    EROFS => (
                        ErrorKind::ReadOnly,
                        format!("The drive is write-protected ({what})."),
                        Some("Check for a lock switch on the drive or SD adapter."),
                    ),
                    _ => (ErrorKind::Other, format!("{what}: {err}"), None),
                };
                let mut e = NyError::new(kind, msg);
                if let Some(h) = hint {
                    e = e.with_hint(h);
                }
                return e;
            }
        }
        #[cfg(windows)]
        {
            if let Some(code) = raw {
                let (kind, msg, hint): (ErrorKind, String, Option<&str>) = match code {
                    5 => (
                        ErrorKind::Permission,
                        format!("Access denied while {what}."),
                        Some("Run NyForge as Administrator."),
                    ),
                    19 => (
                        ErrorKind::ReadOnly,
                        format!("The drive is write-protected ({what})."),
                        Some("Check for a lock switch on the drive or SD adapter."),
                    ),
                    32 | 33 => (
                        ErrorKind::Busy,
                        format!("The drive is in use by another program while {what}."),
                        Some("Close File Explorer windows or programs using the drive, then retry."),
                    ),
                    112 => (
                        ErrorKind::NoSpace,
                        format!("The drive ran out of space while {what}."),
                        Some("Use a larger drive, or a smaller image."),
                    ),
                    21 | 1167 | 55 => (
                        ErrorKind::DeviceGone,
                        format!("The drive disappeared while {what}."),
                        Some("Reconnect it (try a different port) and start again."),
                    ),
                    23 | 1117 | 121 | 483 => (
                        ErrorKind::Hardware,
                        format!("The drive reported a hardware error while {what}."),
                        Some("The drive, card reader or cable may be failing. Try another port or reader first."),
                    ),
                    _ => (ErrorKind::Other, format!("{what}: {err}"), None),
                };
                let mut e = NyError::new(kind, msg);
                if let Some(h) = hint {
                    e = e.with_hint(h);
                }
                return e;
            }
        }
        let _ = raw;
        match err.kind() {
            std::io::ErrorKind::NotFound => {
                NyError::new(ErrorKind::InvalidInput, format!("Not found while {what}."))
            }
            std::io::ErrorKind::PermissionDenied => NyError::new(
                ErrorKind::Permission,
                format!("Permission denied while {what}."),
            ),
            std::io::ErrorKind::UnexpectedEof => NyError::new(
                ErrorKind::ImageCorrupt,
                format!("Unexpected end of data while {what}."),
            )
            .with_hint("The file is truncated. Download it again."),
            _ => NyError::new(ErrorKind::Other, format!("{what}: {err}")),
        }
    }
}

impl fmt::Display for NyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)?;
        if let Some(h) = &self.hint {
            write!(f, " ({h})")?;
        }
        Ok(())
    }
}

impl std::error::Error for NyError {}

impl From<std::io::Error> for NyError {
    fn from(e: std::io::Error) -> Self {
        NyError::from_io(&e, "doing I/O")
    }
}

/// Convenience: `io_result.ctx("reading the image")?`
pub trait IoContext<T> {
    fn ctx(self, what: &str) -> Result<T>;
}

impl<T> IoContext<T> for std::io::Result<T> {
    fn ctx(self, what: &str) -> Result<T> {
        self.map_err(|e| NyError::from_io(&e, what))
    }
}
