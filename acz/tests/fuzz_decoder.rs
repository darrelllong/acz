// Copyright (c) 2026 Darrell D. E. Long. All rights reserved.
// Use and redistribution are subject to the BSD 3-clause license, with a
// dedication clause, in the LICENSE file at the root of this repository.
//
// Dedicated to the memory of my colleague Glen G. Langdon

//! Mutation fuzzing of the decoder: whatever the input, decompression must
//! return either the original data or an error, and never panic.
//!
//! Mutated streams are optionally re-sealed (block and header CRCs
//! recomputed) so that mutations reach the model decoders instead of being
//! stopped by the checksums.
//!
//! The default run is short; set `ACZ_FUZZ_ITERS` for a long run.

use std::panic::{catch_unwind, AssertUnwindSafe};

use acz::crc64::Crc64;
use acz::format::{DecodeOptions, DEFAULT_MEMORY};
use acz::{compress, decompress, Options, Order};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

const HEADER_LEN: usize = 16;
const BLOCK_HEADER_LEN: usize = 9;

/// Recompute the header CRC and every block CRC (as an attacker could), so
/// that only the content is wrong. Stops at the first framing it cannot
/// follow.
fn reseal(v: &mut [u8]) {
    if v.len() < HEADER_LEN {
        return;
    }
    let c = Crc64::of(&v[..8]);
    v[8..16].copy_from_slice(&c.to_le_bytes());
    let mut pos = HEADER_LEN;
    let mut index = 0u64;
    while pos + BLOCK_HEADER_LEN <= v.len() && v[pos] != 0 {
        let packed = u32::from_le_bytes(v[pos + 5..pos + 9].try_into().unwrap()) as usize;
        let end = pos + BLOCK_HEADER_LEN + packed;
        if end + 8 > v.len() {
            return;
        }
        let mut crc = Crc64::new();
        crc.update(&index.to_le_bytes());
        crc.update(&v[pos..end]);
        let c = crc.value();
        v[end..end + 8].copy_from_slice(&c.to_le_bytes());
        pos = end + 8;
        index += 1;
    }
}

fn sample(r: &mut Rng) -> Vec<u8> {
    let n = r.below(3000);
    let alphabet = 1 + r.below(255);
    let mut v: Vec<u8> = (0..n).map(|_| (r.next() % alphabet as u64) as u8).collect();
    // Some long repeats so the dedup path produces copies.
    if n > 600 && r.below(2) == 0 {
        let a = r.below(n - 300);
        let chunk = v[a..a + 300].to_vec();
        v.extend_from_slice(&chunk);
    }
    v
}

#[test]
fn decoder_never_panics_on_mutated_streams() {
    let iters: usize = std::env::var("ACZ_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(3000);
    let mut r = Rng(0x9E37_79B9_7F4A_7C15);
    let orders = [Order::Zero, Order::One, Order::Two, Order::Ppm, Order::Mix];
    let dopts = DecodeOptions { threads: 2, memory: DEFAULT_MEMORY };
    let mut stats = [0usize; 3]; // ok-identical, error, ok-but-different
    for i in 0..iters {
        let data = sample(&mut r);
        // CM allocates ~150 MB per block; use it on a fraction of cases.
        let order = if i % 20 == 0 { Order::Mix } else { orders[r.below(4)] };
        let dedup = if r.below(2) == 0 { 20 } else { 0 };
        let o = Options { order, block_log2: 16, threads: 1, memory: DEFAULT_MEMORY, dedup_log2: dedup };
        let mut packed = Vec::new();
        compress(&data[..], &mut packed, &o).unwrap();
        let mut m = packed.clone();
        for _ in 0..1 + r.below(4) {
            match r.below(5) {
                0 if !m.is_empty() => {
                    let k = r.below(m.len());
                    m[k] ^= 1 << r.below(8);
                }
                1 if !m.is_empty() => {
                    let k = r.below(m.len());
                    m[k] = r.next() as u8;
                }
                2 if !m.is_empty() => {
                    let cut = r.below(m.len());
                    m.truncate(cut);
                }
                3 => {
                    let k = r.below(m.len() + 1);
                    let junk: Vec<u8> = (0..1 + r.below(16)).map(|_| r.next() as u8).collect();
                    m.splice(k..k, junk);
                }
                _ if m.len() > 2 => {
                    let a = r.below(m.len());
                    let b = a + r.below(m.len() - a);
                    m.drain(a..b);
                }
                _ => {}
            }
        }
        if r.below(2) == 0 {
            reseal(&mut m);
        }
        let result = catch_unwind(AssertUnwindSafe(|| {
            let mut out = Vec::new();
            decompress(&m[..], &mut out, &dopts).map(|_| out)
        }));
        match result {
            Err(_) => panic!("decoder panicked (case {i}, {order:?}, dedup {dedup}, input {} bytes)", m.len()),
            Ok(Ok(out)) if out == data => stats[0] += 1,
            Ok(Err(_)) => stats[1] += 1,
            Ok(Ok(_)) => stats[2] += 1,
        }
    }
    eprintln!("identical {} / rejected {} / accepted-but-different {}", stats[0], stats[1], stats[2]);
    // Without resealing, the data CRC must catch every wrong output; with
    // resealing, a wrong output still fails the data CRC in the trailer.
    assert_eq!(stats[2], 0, "a mutated stream decoded to different data without an error");
}
