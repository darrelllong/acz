// Copyright (c) 2026 Darrell D. E. Long. All rights reserved.
// Use and redistribution are subject to the BSD 3-clause license, with a
// dedication clause, in the LICENSE file at the root of this repository.
//
// Dedicated to the memory of my colleague Glen G. Langdon

//! Compare the bit-wise and byte-wise coders on one recorded probability
//! stream (same events, same probabilities): size and throughput.
//!
//! `cargo run --release --example coder_bench`

use std::time::Instant;

use arith::{Decoder, Encoder, RangeDecoder, RangeEncoder};

fn main() {
    // Skewed, realistic stream: 20 M binary events with probabilities from
    // a slowly drifting source.
    let mut s = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = || {
        s ^= s >> 12;
        s ^= s << 25;
        s ^= s >> 27;
        s.wrapping_mul(0x2545_F491_4F6C_DD1D)
    };
    let n = 20_000_000usize;
    let ev: Vec<(bool, u16)> = (0..n)
        .map(|i| {
            let p1 = (((i / 1000) % 60) as u16 * 1000 + 1000).min(65000);
            ((next() % 65536) < u64::from(p1), p1)
        })
        .collect();
    let ideal: f64 = ev
        .iter()
        .map(|&(b, p)| {
            let count = if b { u32::from(p) } else { 65536 - u32::from(p) };
            -(f64::from(count) / 65536.0).log2()
        })
        .sum();

    let t = Instant::now();
    let mut e = Encoder::new(Vec::new());
    for &(b, p) in &ev {
        e.encode_bit_p(b, p).unwrap();
    }
    let a = e.finish().unwrap();
    let ta = t.elapsed();
    let t = Instant::now();
    let mut d = Decoder::new(&a[..]).unwrap();
    let mut ok = true;
    for &(b, p) in &ev {
        ok &= d.decode_bit_p(p).unwrap() == b;
    }
    let tda = t.elapsed();

    let t = Instant::now();
    let mut e = RangeEncoder::new(Vec::new());
    for &(b, p) in &ev {
        e.encode_bit_p(b, p).unwrap();
    }
    let r = e.finish().unwrap();
    let tr = t.elapsed();
    let t = Instant::now();
    let mut d = RangeDecoder::new(&r).unwrap();
    for &(b, p) in &ev {
        ok &= d.decode_bit_p(p) == b;
    }
    d.finish().unwrap();
    let tdr = t.elapsed();
    assert!(ok);

    let mev = |d: std::time::Duration| n as f64 / d.as_secs_f64() / 1e6;
    println!("events: {n}, ideal {:.0} bytes", ideal / 8.0);
    println!(
        "bit-wise   : {} bytes (+{:.4}%)  encode {:.0} Mev/s  decode {:.0} Mev/s",
        a.len(),
        (a.len() as f64 * 8.0 / ideal - 1.0) * 100.0,
        mev(ta),
        mev(tda)
    );
    println!(
        "byte-wise  : {} bytes (+{:.4}%)  encode {:.0} Mev/s  decode {:.0} Mev/s",
        r.len(),
        (r.len() as f64 * 8.0 / ideal - 1.0) * 100.0,
        mev(tr),
        mev(tdr)
    );
}
