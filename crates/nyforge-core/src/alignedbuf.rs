//! Page-aligned buffers: required for O_DIRECT / FILE_FLAG_NO_BUFFERING I/O.

use std::alloc::{alloc_zeroed, dealloc, Layout};

pub const ALIGN: usize = 4096;

pub struct AlignedBuf {
    ptr: *mut u8,
    cap: usize,
}

// The buffer is a plain heap allocation with unique ownership.
unsafe impl Send for AlignedBuf {}
unsafe impl Sync for AlignedBuf {}

impl AlignedBuf {
    pub fn new(cap: usize) -> Self {
        assert!(cap > 0 && cap % ALIGN == 0, "buffer size must be a multiple of {ALIGN}");
        let layout = Layout::from_size_align(cap, ALIGN).expect("layout");
        let ptr = unsafe { alloc_zeroed(layout) };
        assert!(!ptr.is_null(), "out of memory allocating {cap} bytes");
        Self { ptr, cap }
    }

    pub fn capacity(&self) -> usize {
        self.cap
    }

    pub fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.cap) }
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.cap) }
    }
}

impl Drop for AlignedBuf {
    fn drop(&mut self) {
        let layout = Layout::from_size_align(self.cap, ALIGN).expect("layout");
        unsafe { dealloc(self.ptr, layout) }
    }
}

/// Is every byte zero? (Vectorises well; fast enough to run on every chunk.)
pub fn is_all_zero(buf: &[u8]) -> bool {
    let (head, mid, tail) = unsafe { buf.align_to::<u128>() };
    head.iter().all(|&b| b == 0) && mid.iter().all(|&w| w == 0) && tail.iter().all(|&b| b == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligned_and_zeroed() {
        let b = AlignedBuf::new(8192);
        assert_eq!(b.as_slice().as_ptr() as usize % ALIGN, 0);
        assert!(is_all_zero(b.as_slice()));
    }

    #[test]
    fn zero_detection() {
        let mut v = vec![0u8; 100_003];
        assert!(is_all_zero(&v));
        v[77_777] = 1;
        assert!(!is_all_zero(&v));
        assert!(is_all_zero(&v[..77_777]));
        assert!(is_all_zero(&[]));
    }
}
