//! Byte-addressed access to "a region of a disk": a whole image file, or one
//! partition of a drive.  All the NyBoot code (exFAT, FAT, MBR) talks to this
//! trait, so it runs the same on files, Linux block devices and Windows disks.

use crate::alignedbuf::AlignedBuf;
use crate::sys::{read_exact_at, write_all_at};
use std::fs::File;
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

pub trait Io {
    fn len(&self) -> u64;
    fn read_at(&self, buf: &mut [u8], off: u64) -> io::Result<()>;
    fn write_at(&self, buf: &[u8], off: u64) -> io::Result<()>;
    fn sync(&self) -> io::Result<()>;

    fn zero(&self, mut off: u64, mut len: u64) -> io::Result<()> {
        let z = vec![0u8; 1 << 20];
        while len > 0 {
            let n = len.min(z.len() as u64) as usize;
            self.write_at(&z[..n], off)?;
            off += n as u64;
            len -= n as u64;
        }
        Ok(())
    }
}

/// What to do when a read or write fails because the drive dropped out: called with the number of recoveries already
/// tried for this request and the error. `Some(Ok(file))` is a fresh handle to the very same drive (the request is
/// repeated with it), `Some(Err(_))` ends the request with that error, `None` gives up with the original error.
pub type Recovery = Arc<dyn Fn(u32, &io::Error) -> Option<io::Result<File>> + Send + Sync>;

/// Largest single request sent to a raw drive. After a dropout it is halved (down to 32 KiB): some sticks and USB
/// controllers reset under big transfers.
const MAX_IO: usize = 1 << 20;
const MIN_IO: usize = 32 << 10;

/// A window `[base, base+len)` into an open file or device.
pub struct FileIo {
    file: Arc<RwLock<File>>,
    base: u64,
    len: u64,
    /// I/O granularity the handle demands (1 for ordinary files, 4096 for raw
    /// Windows disks and `O_DIRECT`). Unaligned requests are widened.
    align: u64,
    recover: Option<Recovery>,
    max_io: Arc<AtomicUsize>,
}

impl FileIo {
    pub fn new(file: File, base: u64, len: u64, align: u64) -> Self {
        FileIo { file: Arc::new(RwLock::new(file)), base, len, align: align.max(1), recover: None, max_io: Arc::new(AtomicUsize::new(MAX_IO)) }
    }

    /// Pick the drive up again (through `recover`) when it drops out in the middle of a request.
    pub fn with_recovery(mut self, recover: Recovery) -> Self {
        self.recover = Some(recover);
        self
    }

    /// For a real drive on this machine: pick it up again (same drive only) if it drops out. Images files and
    /// drives that are not known stay as they are.
    pub fn recovering(self, path: &std::path::Path, drive: Option<&crate::device::Drive>, how: crate::sys::Reopen) -> FileIo {
        match drive {
            Some(d) => self.with_recovery(crate::sys::drive_recovery(path.to_path_buf(), d.clone(), how, None)),
            None => self,
        }
    }

    /// Same file, narrower window (offsets relative to this window). Shares the handle, and so a recovery.
    pub fn sub(&self, off: u64, len: u64) -> io::Result<FileIo> {
        assert!(off + len <= self.len, "sub-window outside parent");
        Ok(FileIo { file: self.file.clone(), base: self.base + off, len, align: self.align, recover: self.recover.clone(), max_io: self.max_io.clone() })
    }

    /// The open file (for system calls that need the handle itself).
    pub fn file(&self) -> std::sync::RwLockReadGuard<'_, File> {
        self.file.read().unwrap_or_else(|e| e.into_inner())
    }

    fn check(&self, off: u64, n: usize) -> io::Result<()> {
        if off.checked_add(n as u64).map_or(true, |e| e > self.len) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "access outside the partition"));
        }
        Ok(())
    }

    /// Run `op` on the file; if it fails because the drive dropped out and a recovery is set, swap in the fresh
    /// handle, use smaller requests from now on, and run it again.
    fn with_handle<T>(&self, mut op: impl FnMut(&File) -> io::Result<T>) -> io::Result<T> {
        let mut attempt = 0u32;
        loop {
            let r = {
                let f = self.file.read().unwrap_or_else(|e| e.into_inner());
                op(&f)
            };
            let e = match r {
                Ok(v) => return Ok(v),
                Err(e) => e,
            };
            let Some(rec) = &self.recover else { return Err(e) };
            match rec(attempt, &e) {
                Some(Ok(fresh)) => {
                    *self.file.write().unwrap_or_else(|e| e.into_inner()) = fresh;
                    let cur = self.max_io.load(Ordering::Relaxed);
                    self.max_io.store((cur / 2).max(MIN_IO), Ordering::Relaxed);
                    attempt += 1;
                }
                Some(Err(e2)) => return Err(e2),
                None => return Err(e),
            }
        }
    }

    /// Write whole, aligned `data` at `at`, in requests of at most `max_io` bytes.
    fn write_pieces(&self, data: &[u8], at: u64) -> io::Result<()> {
        let mut done = 0usize;
        while done < data.len() {
            let piece = if self.align > 1 { self.max_io.load(Ordering::Relaxed).max(MIN_IO) } else { usize::MAX };
            let n = (data.len() - done).min(piece);
            let off = at + done as u64;
            let part = &data[done..done + n];
            self.with_handle(|f| write_all_at(f, part, off))?;
            done += n;
        }
        Ok(())
    }
}

impl Io for FileIo {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_at(&self, buf: &mut [u8], off: u64) -> io::Result<()> {
        self.check(off, buf.len())?;
        if self.align == 1 {
            return self.with_handle(|f| read_exact_at(f, buf, self.base + off));
        }
        let a = self.align;
        let start = (self.base + off) / a * a;
        let end = (self.base + off + buf.len() as u64).div_ceil(a) * a;
        let span = (end - start) as usize;
        let mut tmp = AlignedBuf::new(span.next_multiple_of(crate::alignedbuf::ALIGN));
        self.with_handle(|f| read_exact_at(f, &mut tmp.as_mut_slice()[..span], start))?;
        let skip = (self.base + off - start) as usize;
        buf.copy_from_slice(&tmp.as_slice()[skip..skip + buf.len()]);
        Ok(())
    }

    fn write_at(&self, buf: &[u8], off: u64) -> io::Result<()> {
        self.check(off, buf.len())?;
        if buf.is_empty() {
            return Ok(());
        }
        if self.align == 1 {
            return self.with_handle(|f| write_all_at(f, buf, self.base + off));
        }
        let a = self.align;
        let start = (self.base + off) / a * a;
        let end = (self.base + off + buf.len() as u64).div_ceil(a) * a;
        let span = (end - start) as usize;
        let mut tmp = AlignedBuf::new(span.next_multiple_of(crate::alignedbuf::ALIGN));
        let skip = (self.base + off - start) as usize;
        if skip != 0 || skip + buf.len() != span {
            // Partial first/last block: keep the neighbouring bytes.
            let _ = self.with_handle(|f| read_exact_at(f, &mut tmp.as_mut_slice()[..span], start));
        }
        tmp.as_mut_slice()[skip..skip + buf.len()].copy_from_slice(buf);
        self.write_pieces(&tmp.as_slice()[..span], start)
    }

    fn sync(&self) -> io::Result<()> {
        self.with_handle(crate::sys::sync_device)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    fn scratch(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("nyforge-io-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_drive_that_drops_out_is_picked_up_again_and_the_request_repeated_in_smaller_pieces() {
        let d = scratch("recover");
        let path = d.join("disk.img");
        std::fs::write(&path, vec![0u8; 8 << 20]).unwrap();
        // The first handle cannot write (a drive that is gone); recovery hands out a good one.
        let bad = File::open(&path).unwrap();
        let tries = Arc::new(AtomicU32::new(0));
        let (p2, t2) = (path.clone(), tries.clone());
        let rec: Recovery = Arc::new(move |_attempt, _e| {
            t2.fetch_add(1, Ordering::SeqCst);
            Some(std::fs::OpenOptions::new().read(true).write(true).open(&p2))
        });
        let io = FileIo::new(bad, 0, 8 << 20, 4096).with_recovery(rec);
        let part = io.sub(1 << 20, 4 << 20).unwrap();
        let data = vec![0xABu8; 3 << 20];
        part.write_at(&data, 4096).unwrap();
        assert_eq!(tries.load(Ordering::SeqCst), 1, "one recovery, shared by the sub-window");
        let on_disk = std::fs::read(&path).unwrap();
        let at = (1 << 20) + 4096;
        assert!(on_disk[at..at + (3 << 20)].iter().all(|&b| b == 0xAB));
        assert_eq!(on_disk[at - 1], 0);
        assert_eq!(io.max_io.load(Ordering::Relaxed), MAX_IO / 2, "requests are smaller from now on");
        // Reading works through the new handle too.
        let mut back = vec![0u8; 16];
        part.read_at(&mut back, 4096).unwrap();
        assert_eq!(back, vec![0xAB; 16]);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn without_a_recovery_or_when_it_says_no_the_error_comes_out() {
        let d = scratch("norecover");
        let path = d.join("disk.img");
        std::fs::write(&path, vec![0u8; 1 << 20]).unwrap();
        let io = FileIo::new(File::open(&path).unwrap(), 0, 1 << 20, 4096);
        assert!(io.write_at(&[1u8; 4096], 0).is_err());
        let no: Recovery = Arc::new(|_, _| None);
        let io = FileIo::new(File::open(&path).unwrap(), 0, 1 << 20, 4096).with_recovery(no);
        assert!(io.write_at(&[1u8; 4096], 0).is_err());
        let failing: Recovery = Arc::new(|_, _| Some(Err(io::Error::other("did not come back"))));
        let io = FileIo::new(File::open(&path).unwrap(), 0, 1 << 20, 4096).with_recovery(failing);
        assert_eq!(io.write_at(&[1u8; 4096], 0).unwrap_err().to_string(), "did not come back");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_big_request_to_a_raw_drive_is_written_in_pieces_and_arrives_whole() {
        let d = scratch("split");
        let path = d.join("disk.img");
        std::fs::write(&path, vec![0u8; 6 << 20]).unwrap();
        let f = std::fs::OpenOptions::new().read(true).write(true).open(&path).unwrap();
        let io = FileIo::new(f, 0, 6 << 20, 4096);
        io.write_at(&vec![5u8; 5 << 20], 0).unwrap();
        assert!(std::fs::read(&path).unwrap()[..5 << 20].iter().all(|&b| b == 5));
        std::fs::remove_dir_all(&d).ok();
    }
}
