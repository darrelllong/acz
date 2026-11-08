// Copyright (c) 2026 Darrell D. E. Long. All rights reserved.
// Use and redistribution are subject to the BSD 3-clause license, with a
// dedication clause, in the LICENSE file at the root of this repository.
//
// Dedicated to the memory of my colleague Glen G. Langdon

//! Check the PPMd port against reference vectors made with 7-Zip's Ppmd7z:
//! `vectors.tsv` (case, order, memory, raw, packed, restarts, ...,
//! payload file) beside `inputs/CASE.bin` and `payloads/`.
//!
//! ```sh
//! cargo run --release --example ppmd_vectors -- DIR
//! ```

use acz::ppmd::{Ppmd, RangeDecoder, RangeEncoder};

fn main() {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).expect("usage: ppmd_vectors DIR"));
    let table = std::fs::read_to_string(dir.join("vectors.tsv")).expect("vectors.tsv");
    let (mut pass, mut fail) = (0, 0);
    for line in table.lines().skip(1) {
        let f: Vec<&str> = line.split('\t').collect();
        let (case, order, mem): (&str, u32, u32) = (f[0], f[1].parse().unwrap(), f[2].parse().unwrap());
        let restarts: u32 = f[5].parse().unwrap();
        let input = std::fs::read(dir.join("inputs").join(format!("{case}.bin"))).expect("input");
        let want = std::fs::read(dir.join("payloads").join(f[8])).expect("payload");
        let mut m = Ppmd::new(order, mem);
        let mut rc = RangeEncoder::new(usize::MAX);
        for &b in &input {
            m.encode(&mut rc, b);
        }
        let got = rc.finish().expect("no limit");
        let mut d = Ppmd::new(order, mem);
        let mut rd = RangeDecoder::new(&want).expect("start");
        let back: Option<Vec<u8>> = (0..input.len()).map(|_| d.decode(&mut rd)).collect();
        let ok = got == want && back.as_deref() == Some(&input[..]) && rd.finish() && m.restarts() == restarts;
        if ok {
            pass += 1;
        } else {
            fail += 1;
            println!(
                "FAIL {case} o{order} m{mem}: bytes {} want {} same={} decode={} finish={} restarts {} want {restarts}",
                got.len(),
                want.len(),
                got == want,
                back.as_deref() == Some(&input[..]),
                rd.finish(),
                m.restarts()
            );
        }
    }
    println!("{pass} passed, {fail} failed");
    std::process::exit(i32::from(fail != 0));
}
