//! A small, fast, non-cryptographic generator (xoshiro256**). Used for wipe patterns and the
//! capacity test, where speed matters (USB sticks write at tens of MB/s, this makes GB/s) and
//! unpredictability does not.

pub struct Rng {
    s: [u64; 4],
}

fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        let mut st = seed;
        Rng { s: [splitmix(&mut st), splitmix(&mut st), splitmix(&mut st), splitmix(&mut st)] }
    }

    pub fn next_u64(&mut self) -> u64 {
        let r = self.s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = self.s[1] << 17;
        self.s[2] ^= self.s[0];
        self.s[3] ^= self.s[1];
        self.s[1] ^= self.s[2];
        self.s[0] ^= self.s[3];
        self.s[2] ^= t;
        self.s[3] = self.s[3].rotate_left(45);
        r
    }

    pub fn fill(&mut self, buf: &mut [u8]) {
        let mut chunks = buf.chunks_exact_mut(8);
        for c in &mut chunks {
            c.copy_from_slice(&self.next_u64().to_le_bytes());
        }
        let rest = chunks.into_remainder();
        if !rest.is_empty() {
            let b = self.next_u64().to_le_bytes();
            rest.copy_from_slice(&b[..rest.len()]);
        }
    }
}

/// Bytes for "block number `index`" of a test run: the same seed and index always give the same bytes,
/// and different indexes give unrelated bytes, so a block that shows up at the wrong place is noticed.
pub fn block_pattern(seed: u64, index: u64, buf: &mut [u8]) {
    let mut st = seed ^ index.wrapping_mul(0xD6E8_FEB8_6659_FD93);
    let key = splitmix(&mut st);
    Rng::new(key).fill(buf);
}

pub fn random_seed() -> u64 {
    let mut b = [0u8; 8];
    let _ = getrandom::fill(&mut b);
    u64::from_le_bytes(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_bytes_different_index_different_bytes() {
        let (mut a, mut b, mut c) = (vec![0u8; 4096], vec![0u8; 4096], vec![0u8; 4096]);
        block_pattern(7, 3, &mut a);
        block_pattern(7, 3, &mut b);
        block_pattern(7, 4, &mut c);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(a.iter().any(|&x| x != 0));
    }

    #[test]
    fn fill_handles_odd_lengths() {
        let mut r = Rng::new(1);
        let mut v = vec![0u8; 13];
        r.fill(&mut v);
        assert!(v.iter().any(|&x| x != 0));
    }
}
