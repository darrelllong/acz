// Copyright (c) 2026 Darrell D. E. Long. All rights reserved.
// Use and redistribution are subject to the BSD 3-clause license, with a
// dedication clause, in the LICENSE file at the root of this repository.
//
// Dedicated to the memory of my colleague Glen G. Langdon

//! CRC-64/XZ (ECMA-182 polynomial, reflected, init and xorout all ones),
//! the CRC-64 used by xz. Polynomial `0x42F0E1EBA9EA3693`, reflected
//! `0xC96C5795D7870F42`. Check value for `"123456789"`:
//! `0x995DC9BBDF1939FA`.

const POLY: u64 = 0xC96C_5795_D787_0F42;

/// Slicing-by-8 tables (M. E. Kounavis and F. L. Berry, *A Systematic
/// Approach to Building High Performance Software-Based CRC Generators*,
/// Proc. IEEE Symposium on Computers and Communications, 2005), built at
/// compile time. `t[0][i]` is the usual
/// one-byte table: the CRC register after shifting byte `i` through it.
/// `t[s][i]` is the same byte followed by `s` zero bytes. The register
/// update is linear over GF(2), so the effect of eight input bytes on the
/// register is the XOR of eight independent lookups. Each byte uses the
/// table for the number of bytes that follow it. That replaces eight
/// dependent lookups with eight that can proceed at once.
const fn make_tables() -> [[u64; 256]; 8] {
    let mut t = [[0u64; 256]; 8];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u64;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { POLY ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[0][i] = c;
        i += 1;
    }
    let mut s = 1;
    while s < 8 {
        let mut i = 0;
        while i < 256 {
            let prev = t[s - 1][i];
            t[s][i] = (prev >> 8) ^ t[0][(prev & 0xFF) as usize];
            i += 1;
        }
        s += 1;
    }
    t
}

static TABLES: [[u64; 256]; 8] = make_tables();

/// An incremental CRC-64.
#[derive(Clone, Copy, Debug)]
pub struct Crc64(u64);

impl Default for Crc64 {
    fn default() -> Self {
        Self::new()
    }
}

impl Crc64 {
    /// Returns the CRC of no bytes.
    pub const fn new() -> Self {
        Crc64(u64::MAX)
    }

    /// Adds `data`.
    pub fn update(&mut self, data: &[u8]) {
        let t = &TABLES;
        let mut c = self.0;
        let mut chunks = data.chunks_exact(8);
        for ch in &mut chunks {
            let v = c ^ u64::from_le_bytes(ch.try_into().expect("8 bytes"));
            c = t[7][(v & 0xFF) as usize]
                ^ t[6][((v >> 8) & 0xFF) as usize]
                ^ t[5][((v >> 16) & 0xFF) as usize]
                ^ t[4][((v >> 24) & 0xFF) as usize]
                ^ t[3][((v >> 32) & 0xFF) as usize]
                ^ t[2][((v >> 40) & 0xFF) as usize]
                ^ t[1][((v >> 48) & 0xFF) as usize]
                ^ t[0][(v >> 56) as usize];
        }
        for &b in chunks.remainder() {
            c = t[0][((c ^ u64::from(b)) & 0xFF) as usize] ^ (c >> 8);
        }
        self.0 = c;
    }

    /// Returns the CRC of the bytes added so far.
    pub fn value(&self) -> u64 {
        !self.0
    }

    /// Returns the CRC of `data`.
    pub fn of(data: &[u8]) -> u64 {
        let mut c = Self::new();
        c.update(data);
        c.value()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_value() {
        assert_eq!(Crc64::of(b"123456789"), 0x995D_C9BB_DF19_39FA);
        assert_eq!(Crc64::of(b""), 0);
    }

    #[test]
    fn slicing_matches_bytewise() {
        let data: Vec<u8> = (0..1000u32).map(|i| (i * 31 + 7) as u8).collect();
        for split in [0, 1, 7, 8, 9, 500, 999, 1000] {
            let mut a = Crc64::new();
            a.update(&data[..split]);
            a.update(&data[split..]);
            let mut b = Crc64::new();
            for byte in &data {
                b.update(std::slice::from_ref(byte));
            }
            assert_eq!(a.value(), b.value());
        }
    }
}
