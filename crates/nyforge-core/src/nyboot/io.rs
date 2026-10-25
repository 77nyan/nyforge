//! Byte-addressed access to "a region of a disk": a whole image file, or one
//! partition of a drive.  All the NyBoot code (exFAT, FAT, MBR) talks to this
//! trait, so it runs the same on files, Linux block devices and Windows disks.

use crate::alignedbuf::AlignedBuf;
use crate::sys::{read_exact_at, write_all_at};
use std::fs::File;
use std::io;

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

/// A window `[base, base+len)` into an open file or device.
pub struct FileIo {
    file: File,
    base: u64,
    len: u64,
    /// I/O granularity the handle demands (1 for ordinary files, 4096 for raw
    /// Windows disks and `O_DIRECT`). Unaligned requests are widened.
    align: u64,
}

impl FileIo {
    pub fn new(file: File, base: u64, len: u64, align: u64) -> Self {
        FileIo { file, base, len, align: align.max(1) }
    }

    /// Same file, narrower window (offsets relative to this window).
    pub fn sub(&self, off: u64, len: u64) -> io::Result<FileIo> {
        assert!(off + len <= self.len, "sub-window outside parent");
        Ok(FileIo { file: self.file.try_clone()?, base: self.base + off, len, align: self.align })
    }

    pub fn file(&self) -> &File {
        &self.file
    }

    fn check(&self, off: u64, n: usize) -> io::Result<()> {
        if off.checked_add(n as u64).map_or(true, |e| e > self.len) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "access outside the partition"));
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
            return read_exact_at(&self.file, buf, self.base + off);
        }
        let a = self.align;
        let start = (self.base + off) / a * a;
        let end = (self.base + off + buf.len() as u64).div_ceil(a) * a;
        let span = (end - start) as usize;
        let mut tmp = AlignedBuf::new(span.next_multiple_of(crate::alignedbuf::ALIGN));
        read_exact_at(&self.file, &mut tmp.as_mut_slice()[..span], start)?;
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
            return write_all_at(&self.file, buf, self.base + off);
        }
        let a = self.align;
        let start = (self.base + off) / a * a;
        let end = (self.base + off + buf.len() as u64).div_ceil(a) * a;
        let span = (end - start) as usize;
        let mut tmp = AlignedBuf::new(span.next_multiple_of(crate::alignedbuf::ALIGN));
        let skip = (self.base + off - start) as usize;
        if skip != 0 || skip + buf.len() != span {
            // Partial first/last block: keep the neighbouring bytes.
            let _ = read_exact_at(&self.file, &mut tmp.as_mut_slice()[..span], start);
        }
        tmp.as_mut_slice()[skip..skip + buf.len()].copy_from_slice(buf);
        write_all_at(&self.file, &tmp.as_slice()[..span], start)
    }

    fn sync(&self) -> io::Result<()> {
        crate::sys::sync_device(&self.file)
    }
}
