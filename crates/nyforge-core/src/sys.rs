//! The thin platform layer under the writer: opening devices exclusively,
//! positional writes, fast zeroing, flushing and re-reading partition tables.
//!
//! One implementation per operating system family (`imp` below): Linux, macOS,
//! other Unix (best effort: images and files only) and Windows.  Everything that
//! can be decided without calling the OS (which device node to open, how a
//! handle must be aligned) is a plain function so it is unit-tested on every
//! platform, not just the one it runs on.

use crate::device::Drive;
use crate::error::{ErrorKind, NyError, Result};
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    BlockDevice,
    File,
}

/// Held for the duration of a write; dropping it releases volume locks (Windows).
#[derive(Default)]
pub struct DevicePrep {
    #[allow(dead_code)]
    pub(crate) guards: Vec<File>,
    pub unmounted: Vec<String>,
}

// ------------------------------------------------------------------- pure helpers

/// The granularity raw disk handles demand on this platform.
///
/// Linux block devices accept any size through the page cache, so files and drives behave the same
/// there.  Windows (`FILE_FLAG_NO_BUFFERING`) and macOS (`/dev/rdiskN`, a raw character device)
/// only accept whole sectors at sector-aligned offsets: callers widen their requests to this.
pub fn block_io_align(kind: TargetKind) -> u64 {
    if kind == TargetKind::BlockDevice && cfg!(any(windows, target_os = "macos")) {
        4096
    } else {
        1
    }
}

/// macOS has two nodes per disk: `/dev/disk4` (buffered, block) and `/dev/rdisk4` (raw, fast).
/// Every tool that moves bulk data uses the raw one, which is about 20× faster for large writes.
pub fn mac_raw_path(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    match s.strip_prefix("/dev/disk") {
        Some(rest) if rest.chars().next().is_some_and(|c| c.is_ascii_digit()) => PathBuf::from(format!("/dev/rdisk{rest}")),
        _ => path.to_path_buf(),
    }
}

/// The reverse of [`mac_raw_path`]: `/dev/rdisk4s1` → `/dev/disk4s1`. Lets people name either node.
pub fn mac_block_path(spec: &str) -> String {
    match spec.strip_prefix("/dev/rdisk") {
        Some(rest) if rest.chars().next().is_some_and(|c| c.is_ascii_digit()) => format!("/dev/disk{rest}"),
        _ => spec.to_string(),
    }
}

/// Flush a file or drive to the hardware.
///
/// Raw disk handles on macOS and Windows sometimes answer the flush request with "not supported" even though
/// nothing is left to flush: they are opened uncached / write-through, so every write has already gone to the
/// device. That answer is not an error. Anything else (a real I/O error) still is.
pub fn sync_device(f: &File) -> io::Result<()> {
    match f.sync_all() {
        Err(e) if e.raw_os_error().is_some_and(|c| flush_error_is_harmless(c, cfg!(target_os = "macos"), cfg!(windows))) => Ok(()),
        r => r,
    }
}

/// The OS error codes that mean "this kind of handle cannot be flushed", per platform.
/// (macOS: ENOTTY 25, ENODEV 19, EINVAL 22, ENOTSUP 45, EOPNOTSUPP 102.
/// Windows: ERROR_INVALID_FUNCTION 1, ERROR_NOT_SUPPORTED 50, ERROR_INVALID_PARAMETER 87.)
pub fn flush_error_is_harmless(code: i32, macos: bool, windows: bool) -> bool {
    (macos && matches!(code, 25 | 19 | 22 | 45 | 102)) || (windows && matches!(code, 1 | 50 | 87))
}

/// What a person must do to get the rights this program needs, in the words of this platform.
pub fn elevation_hint() -> &'static str {
    if cfg!(windows) {
        "Close this window and open Terminal (or PowerShell) with \"Run as administrator\", then run the command again."
    } else {
        "Run the command again with sudo."
    }
}

// ------------------------------------------------------------------- positional I/O

#[cfg(unix)]
pub fn write_all_at(f: &File, mut buf: &[u8], mut off: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    while !buf.is_empty() {
        match f.write_at(buf, off) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::WriteZero, "write returned 0")),
            Ok(n) => {
                buf = &buf[n..];
                off += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(windows)]
pub fn write_all_at(f: &File, mut buf: &[u8], mut off: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let n = f.seek_write(buf, off)?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::WriteZero, "write returned 0"));
        }
        buf = &buf[n..];
        off += n as u64;
    }
    Ok(())
}

#[cfg(unix)]
pub fn read_exact_at(f: &File, buf: &mut [u8], off: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    f.read_exact_at(buf, off)
}

#[cfg(windows)]
pub fn read_exact_at(f: &File, mut buf: &mut [u8], mut off: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let n = f.seek_read(buf, off)?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "read past the end of the device"));
        }
        let rest = std::mem::take(&mut buf);
        buf = &mut rest[n..];
        off += n as u64;
    }
    Ok(())
}

// -------------------------------------------------------------------------- unix

#[cfg(unix)]
mod imp {
    use super::*;
    use std::os::unix::fs::OpenOptionsExt;

    /// The node to open for bulk I/O on `path` (macOS: the raw `rdisk` twin).
    fn io_node(path: &Path) -> PathBuf {
        if cfg!(target_os = "macos") {
            mac_raw_path(path)
        } else {
            path.to_path_buf()
        }
    }

    /// Linux: ask the kernel for an exclusive claim with O_EXCL on a block device; it fails with EBUSY
    /// if anything on it is mounted or in use.  (macOS has no such semantics; there the drive is
    /// released with `diskutil unmountDisk` beforehand.)
    #[cfg(target_os = "linux")]
    const CLAIM: libc::c_int = libc::O_EXCL;
    #[cfg(not(target_os = "linux"))]
    const CLAIM: libc::c_int = 0;

    /// Linux: bypass the page cache. (macOS gets the same effect from `F_NOCACHE` after opening.)
    fn direct_flag(direct: bool) -> libc::c_int {
        #[cfg(target_os = "linux")]
        if direct {
            return libc::O_DIRECT;
        }
        let _ = direct;
        0
    }

    pub fn open_target(path: &Path, kind: TargetKind, direct: bool) -> io::Result<File> {
        match kind {
            TargetKind::File => OpenOptions::new().write(true).create(true).truncate(true).open(path),
            TargetKind::BlockDevice => {
                let flags = libc::O_CLOEXEC | CLAIM | direct_flag(direct);
                let f = OpenOptions::new().write(true).custom_flags(flags).open(io_node(path))?;
                #[cfg(target_os = "macos")]
                if direct {
                    set_direct(&f, true);
                }
                Ok(f)
            }
        }
    }

    pub fn set_direct(file: &File, on: bool) {
        use std::os::fd::AsRawFd;
        #[cfg(target_os = "linux")]
        unsafe {
            let fl = libc::fcntl(file.as_raw_fd(), libc::F_GETFL);
            if fl >= 0 {
                let new = if on { fl | libc::O_DIRECT } else { fl & !libc::O_DIRECT };
                libc::fcntl(file.as_raw_fd(), libc::F_SETFL, new);
            }
        }
        // F_NOCACHE: do not keep this file's data in the unified buffer cache.
        #[cfg(target_os = "macos")]
        unsafe {
            libc::fcntl(file.as_raw_fd(), libc::F_NOCACHE, if on { 1 } else { 0 });
        }
        let _ = (file, on);
    }

    /// Zero a byte range using the device's write-zeroes command. Returns false if unsupported.
    pub fn zero_range(file: &File, off: u64, len: u64) -> bool {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            const BLKZEROOUT: libc::Ioctl = 0x127F;
            let range: [u64; 2] = [off, len];
            let r = unsafe { libc::ioctl(file.as_raw_fd(), BLKZEROOUT, range.as_ptr()) };
            return r == 0;
        }
        #[allow(unreachable_code)]
        {
            let _ = (file, off, len);
            false
        }
    }

    pub fn prepare_device(drive: &Drive, unmount: bool) -> Result<DevicePrep> {
        let mut prep = DevicePrep::default();
        if drive.mounted_points().is_empty() {
            return Ok(prep);
        }
        if !unmount {
            return Err(NyError::new(
                ErrorKind::Busy,
                format!("{} has mounted partitions: {}.", drive.path, drive.mounted_points().join(", ")),
            )
            .with_hint("Unmount them first, or allow NyForge to unmount automatically."));
        }
        prep.unmounted = unmount_drive(drive)?;
        Ok(prep)
    }

    #[cfg(target_os = "linux")]
    fn unmount_drive(drive: &Drive) -> Result<Vec<String>> {
        crate::device::linux::unmount_drive(drive)
    }

    #[cfg(target_os = "macos")]
    fn unmount_drive(drive: &Drive) -> Result<Vec<String>> {
        crate::device::macos::unmount_disk(drive)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn unmount_drive(drive: &Drive) -> Result<Vec<String>> {
        Err(NyError::new(ErrorKind::Unavailable, format!("NyForge cannot unmount {} on this operating system.", drive.path))
            .with_hint("Unmount it yourself first."))
    }

    pub fn finish_device(path: &Path) {
        // Re-read the partition table so the new partitions show up immediately.
        // (macOS notices on its own when the raw device is closed.)
        #[cfg(target_os = "linux")]
        if let Ok(f) = OpenOptions::new().read(true).open(path) {
            crate::device::linux::reread_partition_table(&f);
        }
        let _ = path;
    }

    /// Read+write, exclusive, uncached: every read really comes from the drive, not from RAM.
    /// (A capacity test that is answered from the page cache proves nothing.)
    pub fn open_rw_direct(path: &Path) -> io::Result<File> {
        let mut o = OpenOptions::new();
        o.read(true).write(true).custom_flags(libc::O_CLOEXEC | CLAIM | direct_flag(true));
        let f = o.open(io_node(path))?;
        #[cfg(target_os = "macos")]
        set_direct(&f, true);
        Ok(f)
    }

    /// Open a device for read+write without direct I/O (small metadata writes).
    pub fn open_rw(path: &Path, exclusive: bool) -> io::Result<File> {
        let mut o = OpenOptions::new();
        o.read(true).write(true);
        if exclusive {
            o.custom_flags(libc::O_CLOEXEC | CLAIM);
        }
        o.open(io_node(path))
    }

    pub fn is_root() -> bool {
        unsafe { libc::geteuid() == 0 }
    }
}

// ----------------------------------------------------------------------- windows

#[cfg(windows)]
mod imp {
    use super::*;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::IO::DeviceIoControl;

    const FILE_SHARE_READ: u32 = 1;
    const FILE_SHARE_WRITE: u32 = 2;
    const FILE_FLAG_WRITE_THROUGH: u32 = 0x8000_0000;
    const FILE_FLAG_NO_BUFFERING: u32 = 0x2000_0000;
    const FSCTL_LOCK_VOLUME: u32 = 0x0009_0018;
    const FSCTL_DISMOUNT_VOLUME: u32 = 0x0009_0020;
    const FSCTL_ALLOW_EXTENDED_DASD_IO: u32 = 0x0009_0083;
    const IOCTL_DISK_DELETE_DRIVE_LAYOUT: u32 = 0x0007_C100;
    const IOCTL_DISK_UPDATE_PROPERTIES: u32 = 0x0007_0140;

    fn ioctl(f: &File, code: u32) -> bool {
        let mut returned = 0u32;
        unsafe {
            DeviceIoControl(
                f.as_raw_handle() as _,
                code,
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
                0,
                &mut returned,
                std::ptr::null_mut(),
            ) != 0
        }
    }

    pub fn open_target(path: &Path, kind: TargetKind, direct: bool) -> io::Result<File> {
        match kind {
            TargetKind::File => OpenOptions::new().write(true).create(true).truncate(true).open(path),
            TargetKind::BlockDevice => {
                let mut o = OpenOptions::new();
                o.read(true).write(true).share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE);
                if direct {
                    o.custom_flags(FILE_FLAG_NO_BUFFERING | FILE_FLAG_WRITE_THROUGH);
                } else {
                    o.custom_flags(FILE_FLAG_WRITE_THROUGH);
                }
                let f = o.open(path)?;
                // Allow writes to areas Windows would otherwise consider "outside a volume".
                ioctl(&f, FSCTL_ALLOW_EXTENDED_DASD_IO);
                // Drop the old partition table so Windows stops guarding the sectors.
                ioctl(&f, IOCTL_DISK_DELETE_DRIVE_LAYOUT);
                ioctl(&f, IOCTL_DISK_UPDATE_PROPERTIES);
                Ok(f)
            }
        }
    }

    pub fn set_direct(_file: &File, _on: bool) {}

    pub fn zero_range(_file: &File, _off: u64, _len: u64) -> bool {
        false
    }

    /// Lock and dismount every volume on the drive so Windows lets go of it.
    pub fn prepare_device(drive: &Drive, unmount: bool) -> Result<DevicePrep> {
        let mut prep = DevicePrep::default();
        for mp in drive.mounted_points() {
            if !unmount {
                return Err(NyError::new(ErrorKind::Busy, format!("{} is in use ({mp}).", drive.path))
                    .with_hint("Close programs using the drive, or allow NyForge to dismount it."));
            }
            let letter = mp.trim_end_matches('\\').trim_end_matches(':');
            let vol_path = format!("\\\\.\\{letter}:");
            let f = OpenOptions::new()
                .read(true)
                .write(true)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .open(&vol_path)
                .map_err(|e| NyError::from_io(&e, &format!("opening volume {letter}:")))?;
            if !ioctl(&f, FSCTL_LOCK_VOLUME) {
                return Err(NyError::new(ErrorKind::Busy, format!("Windows would not release {letter}: (a program is still using it)."))
                    .with_hint("Close File Explorer windows and any program showing its files, then retry."));
            }
            ioctl(&f, FSCTL_DISMOUNT_VOLUME);
            prep.unmounted.push(mp.to_string());
            prep.guards.push(f);
        }
        Ok(prep)
    }

    pub fn finish_device(path: &Path) {
        if let Ok(f) = OpenOptions::new().read(true).write(true).share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE).open(path) {
            ioctl(&f, IOCTL_DISK_UPDATE_PROPERTIES);
        }
    }

    /// Read+write and uncached (see the Unix version).
    pub fn open_rw_direct(path: &Path) -> io::Result<File> {
        let mut o = OpenOptions::new();
        o.read(true).write(true).share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE);
        o.custom_flags(FILE_FLAG_NO_BUFFERING | FILE_FLAG_WRITE_THROUGH);
        let f = o.open(path)?;
        ioctl(&f, FSCTL_ALLOW_EXTENDED_DASD_IO);
        ioctl(&f, IOCTL_DISK_DELETE_DRIVE_LAYOUT);
        ioctl(&f, IOCTL_DISK_UPDATE_PROPERTIES);
        Ok(f)
    }

    pub fn open_rw(path: &Path, _exclusive: bool) -> io::Result<File> {
        OpenOptions::new().read(true).write(true).share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE).open(path)
    }

    /// Is this process running with administrator rights (an elevated token)?
    ///
    /// Only a definite "no" returns false: if Windows will not tell us, we let the real
    /// operation decide, because it gives a precise "access denied" of its own.
    pub fn is_root() -> bool {
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
        use windows_sys::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
        unsafe {
            let mut token: HANDLE = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return true;
            }
            let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
            let mut returned = 0u32;
            let ok = GetTokenInformation(
                token,
                TokenElevation,
                &mut elevation as *mut TOKEN_ELEVATION as *mut core::ffi::c_void,
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut returned,
            );
            CloseHandle(token);
            ok == 0 || elevation.TokenIsElevated != 0
        }
    }
}

// ------------------------------------------------------------- anything else (compiles, does little)

#[cfg(not(any(unix, windows)))]
mod imp {
    use super::*;

    fn unsupported() -> io::Error {
        io::Error::new(io::ErrorKind::Unsupported, "this operating system is not supported")
    }

    pub fn open_target(path: &Path, kind: TargetKind, _direct: bool) -> io::Result<File> {
        match kind {
            TargetKind::File => OpenOptions::new().write(true).create(true).truncate(true).open(path),
            TargetKind::BlockDevice => Err(unsupported()),
        }
    }
    pub fn set_direct(_f: &File, _on: bool) {}
    pub fn zero_range(_f: &File, _o: u64, _l: u64) -> bool {
        false
    }
    pub fn prepare_device(_d: &Drive, _u: bool) -> Result<DevicePrep> {
        Err(NyError::new(ErrorKind::Unavailable, "Drives are not supported on this operating system."))
    }
    pub fn finish_device(_p: &Path) {}
    pub fn open_rw_direct(_p: &Path) -> io::Result<File> {
        Err(unsupported())
    }
    pub fn open_rw(p: &Path, _x: bool) -> io::Result<File> {
        OpenOptions::new().read(true).write(true).open(p)
    }
    pub fn is_root() -> bool {
        true
    }
}

/// Does this process have the rights to write to drives (root, or an elevated Administrator)?
pub use imp::is_root;
pub use imp::{finish_device, open_rw, open_rw_direct, open_target, prepare_device, set_direct, zero_range};

#[cfg(not(any(unix, windows)))]
pub fn write_all_at(_f: &File, _buf: &[u8], _off: u64) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "this operating system is not supported"))
}
#[cfg(not(any(unix, windows)))]
pub fn read_exact_at(_f: &File, _buf: &mut [u8], _off: u64) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "this operating system is not supported"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positional_write_roundtrip() {
        let p = std::env::temp_dir().join(format!("nyforge-sys-{}.bin", std::process::id()));
        let f = open_target(&p, TargetKind::File, false).unwrap();
        write_all_at(&f, b"world", 5).unwrap();
        write_all_at(&f, b"hello", 0).unwrap();
        drop(f);
        assert_eq!(std::fs::read(&p).unwrap(), b"helloworld");
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn macos_uses_the_raw_node_for_whole_disks_and_partitions() {
        assert_eq!(mac_raw_path(Path::new("/dev/disk4")), PathBuf::from("/dev/rdisk4"));
        assert_eq!(mac_raw_path(Path::new("/dev/disk12s3")), PathBuf::from("/dev/rdisk12s3"));
        // Already raw, or not a macOS disk node at all: unchanged.
        assert_eq!(mac_raw_path(Path::new("/dev/rdisk4")), PathBuf::from("/dev/rdisk4"));
        assert_eq!(mac_raw_path(Path::new("/dev/sdb")), PathBuf::from("/dev/sdb"));
        assert_eq!(mac_raw_path(Path::new("/dev/disks")), PathBuf::from("/dev/disks"));
        assert_eq!(mac_raw_path(Path::new("/tmp/disk4.img")), PathBuf::from("/tmp/disk4.img"));
    }

    #[test]
    fn either_macos_node_may_be_named() {
        assert_eq!(mac_block_path("/dev/rdisk4"), "/dev/disk4");
        assert_eq!(mac_block_path("/dev/rdisk4s2"), "/dev/disk4s2");
        assert_eq!(mac_block_path("/dev/disk4"), "/dev/disk4");
        assert_eq!(mac_block_path("/dev/rdiskette"), "/dev/rdiskette");
    }

    #[test]
    fn only_raw_disk_handles_need_aligned_requests() {
        assert_eq!(block_io_align(TargetKind::File), 1);
        let drive = block_io_align(TargetKind::BlockDevice);
        if cfg!(any(windows, target_os = "macos")) {
            assert_eq!(drive, 4096);
        } else {
            assert_eq!(drive, 1);
        }
    }

    #[test]
    fn flush_errors_that_only_mean_unsupported_are_ignored() {
        // "Not supported" on a raw disk node is fine; a real I/O error never is.
        assert!(flush_error_is_harmless(25, true, false));
        assert!(flush_error_is_harmless(1, false, true));
        assert!(!flush_error_is_harmless(5, true, false), "EIO on macOS is a real error");
        assert!(!flush_error_is_harmless(5, false, true), "access denied on Windows is a real error");
        assert!(!flush_error_is_harmless(25, false, false), "Linux treats every flush error as real");
        // Ordinary files on this machine flush fine.
        let p = std::env::temp_dir().join(format!("nyforge-sync-{}.bin", std::process::id()));
        let f = open_target(&p, TargetKind::File, false).unwrap();
        sync_device(&f).unwrap();
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn elevation_advice_matches_the_platform() {
        let h = elevation_hint();
        assert_eq!(h.contains("sudo"), !cfg!(windows));
        assert_eq!(h.contains("administrator"), cfg!(windows));
    }
}
