// Copyright (c) 2026 Darrell D. E. Long. All rights reserved.
// Use and redistribution are subject to the BSD 3-clause license, with a
// dedication clause, in the LICENSE file at the root of this repository.
//
// Dedicated to the memory of my colleague Glen G. Langdon

//! Format stability and randomized round-trip tests.
//!
//! `tests/golden/*.acz` are reference v14 streams. Any change to the coder or
//! a model that alters the bitstream of an existing model id makes these
//! tests fail. Until 1.0 the format is unstable and these are regenerated
//! deliberately with each intentional break; at 1.0 they are frozen.
//! Regenerate deliberately with `ACZ_UPDATE_GOLDEN=1 cargo test --test golden`.

use std::fs;
use std::path::PathBuf;

use acz::format::{DecodeOptions, DEFAULT_MEMORY};
use acz::{compress, decompress, Options, Order};

const ORDERS: [(Order, &str); 5] =
    [(Order::Zero, "o0"), (Order::One, "o1"), (Order::Two, "o2"), (Order::Ppm, "ppm"), (Order::Mix, "cm")];

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let x = self.0;
        (x ^ (x >> 29)).wrapping_mul(0xBF58_476D_1CE4_E5B9) >> 16
    }
}

/// Deterministic inputs exercising different statistics.
fn inputs() -> Vec<(&'static str, Vec<u8>)> {
    let mut r = Rng(0xACE1);
    let words = ["arithmetic ", "coding ", "Langdon ", "interval ", "carry ", "the ", "of ", "\n", "1984 "];
    let mut text = Vec::new();
    while text.len() < 150_000 {
        text.extend_from_slice(words[(r.next() % words.len() as u64) as usize].as_bytes());
    }
    let binary: Vec<u8> = (0..20_000).map(|_| r.next() as u8).collect();
    let mut runs = Vec::new();
    while runs.len() < 20_000 {
        let b = (r.next() % 4) as u8;
        let n = 1 + (r.next() % 200) as usize;
        runs.extend(std::iter::repeat(b).take(n));
    }
    vec![("empty", Vec::new()), ("text", text), ("binary", binary), ("runs", runs)]
}

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("golden")
}

#[test]
fn golden_streams_are_stable() {
    let update = std::env::var_os("ACZ_UPDATE_GOLDEN").is_some();
    let dir = golden_dir();
    if update {
        fs::create_dir_all(&dir).unwrap();
    }
    for (name, data) in inputs() {
        for (order, tag) in ORDERS {
            let path = dir.join(format!("{name}.{tag}.acz"));
            let mut packed = Vec::new();
            // Small blocks so the golden files cover multi-block framing.
            compress(
                &data[..],
                &mut packed,
                &Options { order, block_log2: 16, threads: 2, memory: DEFAULT_MEMORY, dedup_log2: 0 },
            )
            .unwrap();
            if update {
                fs::write(&path, &packed).unwrap();
                continue;
            }
            let golden = fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let mut out = Vec::new();
            decompress(&golden[..], &mut out, &DecodeOptions { threads: 2, memory: DEFAULT_MEMORY }).unwrap();
            assert_eq!(out, data, "{} decodes wrongly", path.display());
            assert!(packed == golden, "{}: encoder output changed (format break)", path.display());
        }
    }
}

/// Reference streams with long-repeat removal (format flag + window byte).
#[test]
fn golden_dedup_streams_are_stable() {
    let update = std::env::var_os("ACZ_UPDATE_GOLDEN").is_some();
    let dir = golden_dir();
    let mut data = Vec::new();
    for (_, d) in inputs() {
        data.extend_from_slice(&d);
        data.extend_from_slice(&d);
    }
    for (order, tag) in [(Order::Two, "o2"), (Order::Ppm, "ppm"), (Order::Mix, "cm")] {
        let path = dir.join(format!("repeats.{tag}.dedup.acz"));
        let mut packed = Vec::new();
        let o = Options { order, block_log2: 16, threads: 2, memory: DEFAULT_MEMORY, dedup_log2: 20 };
        compress(&data[..], &mut packed, &o).unwrap();
        if update {
            fs::write(&path, &packed).unwrap();
            continue;
        }
        let golden = fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let mut out = Vec::new();
        decompress(&golden[..], &mut out, &DecodeOptions { threads: 2, memory: DEFAULT_MEMORY }).unwrap();
        assert!(out == data, "{} decodes wrongly", path.display());
        assert!(packed == golden, "{}: encoder output changed", path.display());
    }
}

#[test]
fn fuzz_roundtrip() {
    let mut r = Rng(42);
    let cases = if cfg!(debug_assertions) { 500 } else { 10_000 };
    // CM allocates ~100 MB per stream; fuzz it on fewer cases.
    for case in 0..cases {
        let len = match r.next() % 4 {
            0 => (r.next() % 16) as usize,
            1 => (r.next() % 256) as usize,
            2 => (r.next() % 4096) as usize,
            _ => (r.next() % 20_000) as usize,
        };
        let alphabet = 1 + r.next() % 256;
        let skew = r.next() % 3;
        let data: Vec<u8> = (0..len)
            .map(|_| {
                let x = r.next() % alphabet;
                (if skew == 0 { x } else { x * x / alphabet.max(1) }) as u8
            })
            .collect();
        let orders: &[Order] = if case % 50 == 0 {
            &[Order::Zero, Order::One, Order::Two, Order::Ppm, Order::Mix]
        } else {
            &[Order::Zero, Order::One]
        };
        for &order in orders {
            let mut packed = Vec::new();
            let threads = 1 + case % 3;
            compress(
                &data[..],
                &mut packed,
                &Options { order, block_log2: 16, threads, memory: DEFAULT_MEMORY, dedup_log2: 0 },
            )
            .unwrap();
            let mut out = Vec::new();
            decompress(&packed[..], &mut out, &DecodeOptions { threads, memory: DEFAULT_MEMORY }).unwrap();
            assert_eq!(out, data, "case {case} {order:?} len {len}");
        }
    }
}

/// Streams from earlier, incompatible format versions are kept as evidence
/// and must be rejected cleanly (never misdecoded).
#[test]
fn obsolete_versions_are_rejected() {
    for (dir, version, count) in [
        ("v2", 2, 16),
        ("v3", 3, 16),
        ("v4", 4, 16),
        ("v5", 5, 24),
        ("v6", 6, 18),
        ("v6-lz", 6, 5),
        ("v7", 7, 18),
        ("v8", 8, 18),
        ("v9", 9, 23),
        ("v10", 10, 23),
        ("v11", 11, 23),
        ("v12", 12, 23),
        ("v13", 13, 23),
    ] {
        let d = golden_dir().join("obsolete").join(dir);
        let mut n = 0;
        for entry in fs::read_dir(&d).unwrap() {
            let path = entry.unwrap().path();
            let data = fs::read(&path).unwrap();
            let err = decompress(&data[..], &mut Vec::new(), &DecodeOptions::default()).unwrap_err();
            let want = format!("unsupported acz format version {version}");
            assert!(err.to_string().contains(&want), "{}: {err}", path.display());
            n += 1;
        }
        assert_eq!(n, count, "{dir}: fixtures missing or unexpected");
    }
}

/// Streams written by earlier *encoders* for an unchanged format and model
/// must keep decoding correctly (the encoder may improve; the decoder's
/// interpretation may not change). Files are named `<input>.<model>.<commit>.acz`.
#[test]
fn compatible_older_encoder_streams_still_decode() {
    let inputs: std::collections::HashMap<&str, Vec<u8>> = inputs().into_iter().collect();
    let d = golden_dir().join("compat");
    let Ok(dir) = fs::read_dir(&d) else { return };
    for entry in dir {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        let input = name.split('.').next().unwrap();
        let want = &inputs[input];
        let data = fs::read(&path).unwrap();
        let mut out = Vec::new();
        decompress(&data[..], &mut out, &DecodeOptions::default()).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(&out, want, "{name}");
    }
}
