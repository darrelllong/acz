// Copyright (c) 2026 Darrell D. E. Long. All rights reserved.
// Use and redistribution are subject to the BSD 3-clause license, with a
// dedication clause, in the LICENSE file at the root of this repository.
//
// Dedicated to the memory of my colleague Glen G. Langdon

use std::io::Read;

use arith::{BitModel, Decoder, Encoder, FrequencyTable, Model, MAX_TOTAL};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Sample from a distribution given by counts.
fn sample(rng: &mut Rng, counts: &[u32]) -> usize {
    let total: u64 = counts.iter().map(|&c| u64::from(c)).sum();
    let mut t = rng.below(total);
    for (i, &c) in counts.iter().enumerate() {
        if t < u64::from(c) {
            return i;
        }
        t -= u64::from(c);
    }
    unreachable!()
}

fn entropy_bits(counts: &[u32], n: usize) -> f64 {
    let total: f64 = counts.iter().map(|&c| f64::from(c)).sum();
    let h: f64 = counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = f64::from(c) / total;
            -p * p.log2()
        })
        .sum();
    h * n as f64
}

#[test]
fn static_model_reaches_entropy() {
    let cases: &[&[u32]] = &[&[1, 1], &[1, 99], &[1, 9999], &[5, 0, 3, 0, 1, 1, 7], &[1_000_000, 1, 1, 1], &[1; 256]];
    for counts in cases {
        let mut rng = Rng(0x1234_5678_9abc_def1);
        let n = 200_000;
        let syms: Vec<usize> = (0..n).map(|_| sample(&mut rng, counts)).collect();
        let mut model = FrequencyTable::from_counts(counts);
        let mut enc = Encoder::new(Vec::new());
        for &s in &syms {
            enc.encode_symbol(&mut model, s).unwrap();
        }
        let code = enc.finish().unwrap();

        // Ideal length for the *empirical* sequence under the model.
        let model_total: f64 = f64::from(model.total());
        let ideal: f64 = syms.iter().map(|&s| -(f64::from(model.frequency(s)) / model_total).log2()).sum();
        let actual = code.len() as f64 * 8.0;
        // Overhead: termination (~40 bits) plus truncation loss, which for
        // 32-bit registers is well under 0.01%.
        assert!(
            actual <= ideal * 1.0001 + 48.0,
            "{counts:?}: {actual} bits vs ideal {ideal:.1} (entropy {:.1})",
            entropy_bits(counts, n)
        );

        let mut model = FrequencyTable::from_counts(counts);
        let mut dec = Decoder::new(&code[..]).unwrap();
        for (i, &s) in syms.iter().enumerate() {
            assert_eq!(dec.decode_symbol(&mut model).unwrap(), s, "{counts:?} at {i}");
        }
    }
}

#[test]
fn adaptive_models_roundtrip() {
    for &(alphabet, seed) in &[(2usize, 1u64), (3, 2), (17, 3), (256, 4), (257, 5), (4096, 6), (30000, 7)] {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        // Skewed, drifting source.
        let syms: Vec<usize> = (0..50_000)
            .map(|i| {
                let r = rng.next();
                if r % 4 == 0 {
                    (r >> 8) as usize % alphabet
                } else {
                    ((i / 1000) + (r >> 40) as usize % 3) % alphabet
                }
            })
            .collect();
        let mut m = FrequencyTable::new(alphabet);
        let mut enc = Encoder::new(Vec::new());
        for &s in &syms {
            enc.encode_symbol(&mut m, s).unwrap();
        }
        let code = enc.finish().unwrap();
        let mut m = FrequencyTable::new(alphabet);
        let mut dec = Decoder::new(&code[..]).unwrap();
        for &s in &syms {
            assert_eq!(dec.decode_symbol(&mut m).unwrap(), s);
        }
    }
}

#[test]
fn raw_intervals_with_max_total() {
    // Drive the coder directly with extreme intervals at MAX_TOTAL.
    let mut rng = Rng(42);
    let mut events = Vec::new();
    for _ in 0..100_000 {
        let total = match rng.below(4) {
            0 => MAX_TOTAL,
            1 => 1 + rng.below(u64::from(MAX_TOTAL)) as u32,
            2 => 2,
            _ => 1 + rng.below(1000) as u32,
        };
        let (lo, hi) = match rng.below(3) {
            0 => (0, 1),
            1 => (total - 1, total),
            _ => {
                let a = rng.below(u64::from(total)) as u32;
                let b = rng.below(u64::from(total)) as u32;
                let (a, b) = (a.min(b), a.max(b) + 1);
                (a, b)
            }
        };
        events.push((lo, hi, total));
    }
    let mut enc = Encoder::new(Vec::new());
    for &(lo, hi, t) in &events {
        enc.encode(lo, hi, t).unwrap();
    }
    let code = enc.finish().unwrap();
    let mut dec = Decoder::new(&code[..]).unwrap();
    for &(lo, hi, t) in &events {
        let x = dec.target(t);
        assert!(lo <= x && x < hi, "target {x} outside [{lo},{hi}) of {t}");
        dec.consume(lo, hi, t).unwrap();
    }
}

#[test]
fn binary_models() {
    for p_one in [0.5, 0.1, 0.01, 0.0001, 0.9999] {
        let mut rng = Rng(7);
        let n = 100_000;
        let bits: Vec<bool> = (0..n).map(|_| (rng.next() as f64 / u64::MAX as f64) < p_one).collect();
        let mut m = BitModel::<5>::new();
        let mut enc = Encoder::new(Vec::new());
        for &b in &bits {
            enc.encode_bit(&mut m, b).unwrap();
        }
        let code = enc.finish().unwrap();
        let h = -(p_one * f64::log2(p_one) + (1.0 - p_one) * f64::log2(1.0 - p_one)) * n as f64;
        let actual = code.len() as f64 * 8.0;
        // An EWMA estimator is not optimal for a stationary source, but it
        // should be within a modest factor (and absolute slack for very
        // skewed sources, where the probability floor dominates).
        assert!(actual < h * 1.25 + 0.002 * n as f64 + 64.0, "p={p_one}: {actual} vs {h:.0}");

        let mut m = BitModel::<5>::new();
        let mut dec = Decoder::new(&code[..]).unwrap();
        for &b in &bits {
            assert_eq!(dec.decode_bit(&mut m).unwrap(), b);
        }
    }
}

#[test]
fn decoder_stops_exactly_at_end_of_code() {
    // The decoder must not consume any byte past the code string.
    let mut rng = Rng(99);
    for len in 0..500 {
        let syms: Vec<usize> = (0..len).map(|_| rng.below(5) as usize).collect();
        let mut m = FrequencyTable::new(5);
        let mut enc = Encoder::new(Vec::new());
        for &s in &syms {
            enc.encode_symbol(&mut m, s).unwrap();
        }
        let mut stream = enc.finish().unwrap();
        stream.extend_from_slice(b"TRAILER");

        let mut m = FrequencyTable::new(5);
        let mut dec = Decoder::new(&stream[..]).unwrap();
        for &s in &syms {
            assert_eq!(dec.decode_symbol(&mut m).unwrap(), s);
        }
        assert!(dec.is_canonical_end());
        let mut rest = Vec::new();
        dec.finish().read_to_end(&mut rest).unwrap();
        assert_eq!(rest, b"TRAILER", "len {len}");
    }
}

#[test]
fn truncated_input_errors() {
    let mut m = FrequencyTable::new(256);
    let mut enc = Encoder::new(Vec::new());
    for i in 0..1000 {
        enc.encode_symbol(&mut m, (i * 7) % 256).unwrap();
    }
    let code = enc.finish().unwrap();
    let cut = &code[..code.len() - 2];
    let mut m = FrequencyTable::new(256);
    let r: Result<Vec<usize>, _> =
        Decoder::new(cut).and_then(|mut d| (0..1000).map(|_| d.decode_symbol(&mut m)).collect());
    assert_eq!(r.unwrap_err().kind(), std::io::ErrorKind::UnexpectedEof);
}

#[test]
fn many_pending_bits() {
    // Repeatedly choosing the interval just around the midpoint maximizes
    // pending (carry) bits.
    let total = 1 << 16;
    let events: Vec<(u32, u32)> =
        (0..20_000).map(|i| if i % 2 == 0 { (total / 2 - 1, total / 2) } else { (total / 2, total / 2 + 1) }).collect();
    let mut enc = Encoder::new(Vec::new());
    for &(lo, hi) in &events {
        enc.encode(lo, hi, total).unwrap();
    }
    let code = enc.finish().unwrap();
    let mut dec = Decoder::new(&code[..]).unwrap();
    for &(lo, hi) in &events {
        let x = dec.target(total);
        assert!(lo <= x && x < hi);
        dec.consume(lo, hi, total).unwrap();
    }
}

#[test]
fn custom_model_trait_object() {
    /// Static uniform model over n symbols.
    struct Uniform(usize);
    impl Model for Uniform {
        fn num_symbols(&self) -> usize {
            self.0
        }
        fn total(&self) -> u32 {
            self.0 as u32
        }
        fn interval(&self, s: usize) -> (u32, u32) {
            (s as u32, s as u32 + 1)
        }
        fn find(&self, t: u32) -> (usize, u32, u32) {
            (t as usize, t, t + 1)
        }
        fn update(&mut self, _: usize) {}
    }
    let mut m: Box<dyn Model> = Box::new(Uniform(10));
    let digits = b"31415926535897932384626433832795028841971";
    let mut enc = Encoder::new(Vec::new());
    for d in digits {
        enc.encode_symbol(m.as_mut(), usize::from(d - b'0')).unwrap();
    }
    let code = enc.finish().unwrap();
    // log2(10) * 41 ≈ 136.2 bits ≈ 17 bytes, plus termination.
    assert!(code.len() <= 17 + 5, "{}", code.len());
    let mut dec = Decoder::new(&code[..]).unwrap();
    for &d in digits {
        assert_eq!(dec.decode_symbol(m.as_mut()).unwrap(), usize::from(d - b'0'));
    }
}

#[test]
fn frequency_table_find_matches_interval() {
    let mut rng = Rng(5);
    let mut t = FrequencyTable::new(300);
    for _ in 0..20_000 {
        let s = rng.below(300) as usize;
        t.update(s);
        let (lo, hi) = t.interval(s);
        assert!(lo < hi);
        for target in [lo, hi - 1, (lo + hi) / 2] {
            assert_eq!(t.find(target), (s, lo, hi));
        }
        assert!(t.total() <= FrequencyTable::DEFAULT_LIMIT);
    }
    let (_, hi) = t.interval(299);
    assert_eq!(hi, t.total());
}

#[test]
fn binary_fast_path_is_bit_identical_to_generic_path() {
    let mut rng = Rng(2024);
    let events: Vec<(bool, u16)> = (0..200_000)
        .map(|_| {
            let p1 = match rng.below(4) {
                0 => 1,
                1 => 65535,
                _ => 1 + rng.below(65535) as u16,
            };
            (rng.below(65536) < u64::from(p1), p1)
        })
        .collect();
    let mut fast = Encoder::new(Vec::new());
    let mut slow = Encoder::new(Vec::new());
    for &(bit, p1) in &events {
        fast.encode_bit_p(bit, p1).unwrap();
        let split = 65536 - u32::from(p1);
        if bit {
            slow.encode(split, 65536, 65536).unwrap();
        } else {
            slow.encode(0, split, 65536).unwrap();
        }
    }
    let fast = fast.finish().unwrap();
    assert_eq!(fast, slow.finish().unwrap());
    // Decode with the generic API too.
    let mut dec = Decoder::new(&fast[..]).unwrap();
    for (i, &(bit, p1)) in events.iter().enumerate() {
        let split = 65536 - u32::from(p1);
        let got = if i % 2 == 0 {
            dec.decode_bit_p(p1).unwrap()
        } else {
            let b = dec.target(65536) >= split;
            if b { dec.consume(split, 65536, 65536) } else { dec.consume(0, split, 65536) }.unwrap();
            b
        };
        assert_eq!(got, bit, "event {i}");
    }
    assert!(dec.is_canonical_end());
}

#[test]
fn consume_rejects_interval_not_containing_target() {
    let mut enc = Encoder::new(Vec::new());
    for _ in 0..100 {
        enc.encode(0, 1, 4).unwrap(); // always the first quarter
    }
    let code = enc.finish().unwrap();
    let mut dec = Decoder::new(&code[..]).unwrap();
    assert_eq!(dec.target(4), 0);
    // The last quarter cannot contain the target.
    let err = dec.consume(3, 4, 4).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    // Poisoned: further use fails cleanly instead of panicking.
    let _ = dec.target(4);
    assert!(dec.consume(0, 1, 4).is_err());
    assert!(dec.decode_bit_p(32768).is_err());
    assert!(!dec.is_canonical_end());
}

#[test]
fn encoder_is_poisoned_after_write_error() {
    struct Failing(usize);
    impl std::io::Write for Failing {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            if self.0 == 0 {
                return Err(std::io::Error::other("disk full"));
            }
            self.0 -= 1;
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut enc = Encoder::new(Failing(3));
    let mut failed = false;
    for _ in 0..1000 {
        if enc.encode_bit_p(true, 1).is_err() {
            failed = true;
            break;
        }
    }
    assert!(failed, "sink error was not reported");
    assert!(enc.encode_bit_p(false, 30000).is_err());
    assert!(enc.encode(0, 1, 2).is_err());
    assert!(enc.finish().is_err());
}
