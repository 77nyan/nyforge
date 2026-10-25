//! `Read + Write + Seek` over an [`Io`] region, with a write-back block cache.
//!
//! Filesystem crates (like `fatfs`) issue many small reads and writes. Raw
//! drives want big, aligned requests, so this keeps recently used 64 KiB
//! blocks in memory and writes dirty ones back when evicted or flushed.

use super::io::Io;
use std::collections::HashMap;
use std::io::{self, Read, Seek, SeekFrom, Write};

const BLOCK: u64 = 64 * 1024;
const MAX_BLOCKS: usize = 96; // ~6 MiB

struct Block {
    data: Vec<u8>,
    dirty: bool,
    used: u64,
}

pub struct CachedStream<I: Io> {
    io: I,
    pos: u64,
    blocks: HashMap<u64, Block>,
    tick: u64,
}

impl<I: Io> CachedStream<I> {
    pub fn new(io: I) -> Self {
        CachedStream { io, pos: 0, blocks: HashMap::new(), tick: 0 }
    }

    pub fn io(&self) -> &I {
        &self.io
    }

    fn block_len(&self, idx: u64) -> usize {
        (self.io.len() - idx * BLOCK).min(BLOCK) as usize
    }

    fn write_back(&mut self, idx: u64) -> io::Result<()> {
        if let Some(b) = self.blocks.get_mut(&idx) {
            if b.dirty {
                self.io.write_at(&b.data, idx * BLOCK)?;
                b.dirty = false;
            }
        }
        Ok(())
    }

    fn evict_if_full(&mut self) -> io::Result<()> {
        while self.blocks.len() >= MAX_BLOCKS {
            let oldest = *self.blocks.iter().min_by_key(|(_, b)| b.used).map(|(k, _)| k).unwrap();
            self.write_back(oldest)?;
            self.blocks.remove(&oldest);
        }
        Ok(())
    }

    /// Make block `idx` resident; `fill` = read its current contents (false when it is about to be fully overwritten).
    fn load(&mut self, idx: u64, fill: bool) -> io::Result<()> {
        self.tick += 1;
        if let Some(b) = self.blocks.get_mut(&idx) {
            b.used = self.tick;
            return Ok(());
        }
        self.evict_if_full()?;
        let mut data = vec![0u8; self.block_len(idx)];
        if fill {
            self.io.read_at(&mut data, idx * BLOCK)?;
        }
        self.blocks.insert(idx, Block { data, dirty: false, used: self.tick });
        Ok(())
    }

    pub fn flush_all(&mut self) -> io::Result<()> {
        let mut idxs: Vec<u64> = self.blocks.iter().filter(|(_, b)| b.dirty).map(|(k, _)| *k).collect();
        idxs.sort_unstable();
        for i in idxs {
            self.write_back(i)?;
        }
        Ok(())
    }
}

impl<I: Io> Read for CachedStream<I> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.io.len() || buf.is_empty() {
            return Ok(0);
        }
        let idx = self.pos / BLOCK;
        let off = (self.pos % BLOCK) as usize;
        self.load(idx, true)?;
        let b = &self.blocks[&idx];
        let n = buf.len().min(b.data.len() - off);
        buf[..n].copy_from_slice(&b.data[off..off + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl<I: Io> Write for CachedStream<I> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.pos >= self.io.len() {
            return Err(io::Error::new(io::ErrorKind::WriteZero, "write past the end of the partition"));
        }
        let idx = self.pos / BLOCK;
        let off = (self.pos % BLOCK) as usize;
        let blen = self.block_len(idx);
        let n = buf.len().min(blen - off);
        // A write that covers the whole block does not need the old contents.
        self.load(idx, !(off == 0 && n == blen))?;
        let b = self.blocks.get_mut(&idx).unwrap();
        b.data[off..off + n].copy_from_slice(&buf[..n]);
        b.dirty = true;
        self.pos += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_all()
    }
}

impl<I: Io> Seek for CachedStream<I> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let new = match to {
            SeekFrom::Start(p) => Some(p),
            SeekFrom::End(d) => self.io.len().checked_add_signed(d),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        };
        match new {
            Some(p) => {
                self.pos = p;
                Ok(p)
            }
            None => Err(io::Error::new(io::ErrorKind::InvalidInput, "seek before the start")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct MemIo {
        d: RefCell<Vec<u8>>,
        writes: RefCell<usize>,
    }
    impl Io for MemIo {
        fn len(&self) -> u64 {
            self.d.borrow().len() as u64
        }
        fn read_at(&self, buf: &mut [u8], off: u64) -> io::Result<()> {
            buf.copy_from_slice(&self.d.borrow()[off as usize..off as usize + buf.len()]);
            Ok(())
        }
        fn write_at(&self, buf: &[u8], off: u64) -> io::Result<()> {
            *self.writes.borrow_mut() += 1;
            self.d.borrow_mut()[off as usize..off as usize + buf.len()].copy_from_slice(buf);
            Ok(())
        }
        fn sync(&self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn matches_a_plain_buffer_under_random_access() {
        let size = 1_000_003usize; // not a multiple of the block size
        let io = MemIo { d: RefCell::new(vec![0; size]), writes: RefCell::new(0) };
        let mut s = CachedStream::new(io);
        let mut model = vec![0u8; size];
        let mut x = 99u64;
        for _ in 0..3000 {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let off = (x >> 20) as usize % size;
            let len = ((x >> 8) as usize % 200_000).min(size - off);
            let data: Vec<u8> = (0..len).map(|i| (i as u64 ^ x) as u8).collect();
            s.seek(SeekFrom::Start(off as u64)).unwrap();
            s.write_all(&data).unwrap();
            model[off..off + len].copy_from_slice(&data);
            if x % 7 == 0 {
                let ro = (x >> 13) as usize % size;
                let rl = ((x >> 3) as usize % 100_000).min(size - ro);
                let mut got = vec![0u8; rl];
                s.seek(SeekFrom::Start(ro as u64)).unwrap();
                s.read_exact(&mut got).unwrap();
                assert_eq!(got, model[ro..ro + rl]);
            }
        }
        s.flush().unwrap();
        assert_eq!(*s.io().d.borrow(), model);
    }

    #[test]
    fn small_writes_are_batched() {
        let io = MemIo { d: RefCell::new(vec![0; 4 << 20]), writes: RefCell::new(0) };
        let mut s = CachedStream::new(io);
        for i in 0..10_000u32 {
            s.write_all(&i.to_le_bytes()).unwrap();
        }
        s.flush().unwrap();
        assert!(*s.io().writes.borrow() <= 2, "{} device writes", s.io().writes.borrow());
    }

    #[test]
    fn writing_past_the_end_fails_cleanly() {
        let io = MemIo { d: RefCell::new(vec![0; 1000]), writes: RefCell::new(0) };
        let mut s = CachedStream::new(io);
        s.seek(SeekFrom::Start(990)).unwrap();
        assert!(s.write_all(&[1u8; 20]).is_err());
    }
}
