// Copyright (c) 2026 Darrell D. E. Long. All rights reserved.
// Use and redistribution are subject to the BSD 3-clause license, with a
// dedication clause, in the LICENSE file at the root of this repository.
//
// Dedicated to the memory of my colleague Glen G. Langdon

//! Tests for the byte-wise binary range coder, including a comparison with
//! the bit-wise coder on identical probability streams.

use arith::{BitModel, Decoder, Encoder, RangeDecoder, RangeEncoder};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

/// A recorded stream of (bit, p1) events with varied, extreme probabilities.
fn events(seed: u64, n: usize) -> Vec<(bool, u16)> {
    let mut r = Rng(seed);
    (0..n)
        .map(|_| {
            let p1 = match r.next() % 6 {
                0 => 1,
                1 => 65535,
                2 => 1 + (r.next() % 64) as u16,
                3 => 65535 - (r.next() % 64) as u16,
                _ => 1 + (r.next() % 65535) as u16,
            };
            (r.next() % 65536 < u64::from(p1), p1)
        })
        .collect()
}

#[test]
fn roundtrip_recorded_streams() {
    for seed in 1..20u64 {
        let ev = events(seed, 50_000);
        let mut enc = RangeEncoder::new(Vec::new());
        for &(b, p) in &ev {
            enc.encode_bit_p(b, p).unwrap();
        }
        let code = enc.finish().unwrap();
        let mut dec = RangeDecoder::new(&code).unwrap();
        for (i, &(b, p)) in ev.iter().enumerate() {
            assert_eq!(dec.decode_bit_p(p), b, "seed {seed} event {i}");
        }
        dec.finish().unwrap();
    }
}

#[test]
fn every_short_length_consumes_exactly() {
    for n in 0..400 {
        let ev = events(n as u64 + 100, n);
        let mut enc = RangeEncoder::new(Vec::new());
        for &(b, p) in &ev {
            enc.encode_bit_p(b, p).unwrap();
        }
        let code = enc.finish().unwrap();
        let mut dec = RangeDecoder::new(&code).unwrap();
        for &(b, p) in &ev {
            assert_eq!(dec.decode_bit_p(p), b);
        }
        dec.finish().unwrap_or_else(|e| panic!("n={n}: {e}"));
    }
}

#[test]
fn carry_propagation_through_long_ff_runs() {
    // Highly probable 1s push `low` up repeatedly, creating long runs of
    // pending 0xFF bytes that a later carry must ripple through.
    let mut ev = Vec::new();
    for i in 0..200_000 {
        ev.push((i % 5000 != 0, 65535 - (i % 3) as u16));
    }
    let mut enc = RangeEncoder::new(Vec::new());
    for &(b, p) in &ev {
        enc.encode_bit_p(b, p).unwrap();
    }
    let code = enc.finish().unwrap();
    let mut dec = RangeDecoder::new(&code).unwrap();
    for &(b, p) in &ev {
        assert_eq!(dec.decode_bit_p(p), b);
    }
    dec.finish().unwrap();
}

#[test]
fn both_coders_agree_and_cost_the_same_on_identical_probabilities() {
    // The same recorded probability stream through both coders.
    let ev = events(7, 400_000);
    let ideal: f64 = ev
        .iter()
        .map(|&(b, p)| {
            let p1 = f64::from(p) / 65536.0;
            -(if b { p1 } else { 1.0 - p1 }).log2()
        })
        .sum();
    let mut a = Encoder::new(Vec::new());
    let mut r = RangeEncoder::new(Vec::new());
    for &(b, p) in &ev {
        a.encode_bit_p(b, p).unwrap();
        r.encode_bit_p(b, p).unwrap();
    }
    let a = a.finish().unwrap();
    let r = r.finish().unwrap();
    let (abits, rbits) = (a.len() as f64 * 8.0, r.len() as f64 * 8.0);
    // Both within 0.01% (+ termination) of the ideal code length.
    assert!(abits <= ideal * 1.0001 + 64.0, "bitwise {abits} vs ideal {ideal:.0}");
    assert!(rbits <= ideal * 1.0001 + 64.0, "range {rbits} vs ideal {ideal:.0}");
    let mut da = Decoder::new(&a[..]).unwrap();
    let mut dr = RangeDecoder::new(&r).unwrap();
    for &(b, p) in &ev {
        assert_eq!(da.decode_bit_p(p).unwrap(), b);
        assert_eq!(dr.decode_bit_p(p), b);
    }
    dr.finish().unwrap();
    assert!(da.is_canonical_end());
}

#[test]
fn adaptive_models_and_direct_bits() {
    let mut r = Rng(3);
    let data: Vec<(u32, bool)> = (0..20_000).map(|_| ((r.next() >> 40) as u32, r.next() % 7 == 0)).collect();
    let mut m = [BitModel::<4>::new(); 2];
    let mut enc = RangeEncoder::new(Vec::new());
    for &(v, b) in &data {
        enc.encode_bit(&mut m[0], b).unwrap();
        enc.encode_direct(v & 0xFFFFF, 20).unwrap();
        enc.encode_bit(&mut m[1], !b).unwrap();
    }
    let code = enc.finish().unwrap();
    let mut m = [BitModel::<4>::new(); 2];
    let mut dec = RangeDecoder::new(&code).unwrap();
    for &(v, b) in &data {
        assert_eq!(dec.decode_bit(&mut m[0]), b);
        assert_eq!(dec.decode_direct(20), v & 0xFFFFF);
        assert_eq!(dec.decode_bit(&mut m[1]), !b);
    }
    dec.finish().unwrap();
}

#[test]
fn truncated_and_trailing_input_are_reported() {
    let ev = events(9, 10_000);
    let mut enc = RangeEncoder::new(Vec::new());
    for &(b, p) in &ev {
        enc.encode_bit_p(b, p).unwrap();
    }
    let code = enc.finish().unwrap();
    // Truncated: decoding never panics, and finish reports the overrun.
    let cut = &code[..code.len() - 3];
    let mut dec = RangeDecoder::new(cut).unwrap();
    for &(_, p) in &ev {
        let _ = dec.decode_bit_p(p);
    }
    assert!(dec.finish().is_err());
    // Trailing garbage.
    let mut long = code.clone();
    long.push(0);
    let mut dec = RangeDecoder::new(&long).unwrap();
    for &(_, p) in &ev {
        let _ = dec.decode_bit_p(p);
    }
    assert!(dec.finish().is_err());
    // Bad first byte / too short.
    assert!(RangeDecoder::new(&[1, 0, 0, 0, 0]).is_err());
    assert!(RangeDecoder::new(&[0, 0, 0]).is_err());
}

#[test]
fn non_canonical_termination_is_rejected() {
    // No events, but a non-zero code value.
    let dec = RangeDecoder::new(&[0, 1, 0, 0, 0]).unwrap();
    assert!(dec.finish().is_err());
    let dec = RangeDecoder::new(&[0, 0, 0, 0, 0]).unwrap();
    dec.finish().unwrap();
    // Changing any byte of a valid stream that still decodes the same
    // events must be caught by finish().
    let ev = events(11, 3000);
    let mut enc = RangeEncoder::new(Vec::new());
    for &(b, p) in &ev {
        enc.encode_bit_p(b, p).unwrap();
    }
    let code = enc.finish().unwrap();
    for i in 0..code.len() {
        for bit in 0..8 {
            let mut c = code.clone();
            c[i] ^= 1 << bit;
            let Ok(mut dec) = RangeDecoder::new(&c) else { continue };
            let same = ev.iter().all(|&(b, p)| dec.decode_bit_p(p) == b);
            if same {
                assert!(dec.finish().is_err(), "byte {i} bit {bit}: altered stream accepted");
            }
        }
    }
}

#[test]
#[should_panic(expected = "at most 32 direct bits")]
fn direct_bits_over_32_panic() {
    let mut enc = RangeEncoder::new(Vec::new());
    let _ = enc.encode_direct(0, 33);
}
