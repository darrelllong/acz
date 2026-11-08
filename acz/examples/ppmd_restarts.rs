// Copyright (c) 2026 Darrell D. E. Long. All rights reserved.
// Use and redistribution are subject to the BSD 3-clause license, with a
// dedication clause, in the LICENSE file at the root of this repository.
//
// Dedicated to the memory of my colleague Glen G. Langdon

//! Report how often PPMd (`acz -3`) restarts on each block of a file at the
//! default block size, coding each block as `acz -3` does: a fresh model,
//! and the attempt abandoned once the payload would not be smaller (the
//! block is then stored).
//!
//! ```sh
//! cargo run --release --example ppmd_restarts -- FILE...
//! ```

use std::io::{self, Read};

use acz::ppmd::{arena_size, Ppmd, RangeEncoder, ORDER};

struct Measurement {
    restarts: u32,
    /// Bytes modelled before the attempt finished or was abandoned.
    modeled: usize,
    /// None means `acz -3` stores the block.
    packed: Option<usize>,
}

fn measure(chunk: &[u8]) -> Measurement {
    let Some(limit) = chunk.len().checked_sub(1) else {
        return Measurement { restarts: 0, modeled: 0, packed: None };
    };
    let mut model = Ppmd::new(ORDER, arena_size(chunk.len()));
    let mut rc = RangeEncoder::new(limit);
    let mut modeled = 0;
    for &b in chunk {
        model.encode(&mut rc, b);
        modeled += 1;
        if rc.is_full() {
            return Measurement { restarts: model.restarts(), modeled, packed: None };
        }
    }
    let restarts = model.restarts();
    Measurement { restarts, modeled, packed: rc.finish().map(|p| p.len()) }
}

fn main() -> io::Result<()> {
    let block = 1usize << acz::format::DEFAULT_BLOCK_LOG2;
    println!("file\tblock\tbytes\trestarts\tmodeled_bytes\tstorage\tpayload_bytes");
    let mut chunk = Vec::with_capacity(block);
    for name in std::env::args_os().skip(1) {
        let mut input = std::fs::File::open(&name)?;
        let mut i = 0;
        loop {
            chunk.clear();
            Read::by_ref(&mut input).take(block as u64).read_to_end(&mut chunk)?;
            if chunk.is_empty() {
                break;
            }
            let m = measure(&chunk);
            let storage = if m.packed.is_some() { "compressed" } else { "stored" };
            let name = name.to_string_lossy().replace('\t', "\\t").replace('\n', "\\n").replace('\r', "\\r");
            println!(
                "{name}\t{i}\t{}\t{}\t{}\t{storage}\t{}",
                chunk.len(),
                m.restarts,
                m.modeled,
                m.packed.unwrap_or(chunk.len())
            );
            i += 1;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use acz::models::{encode_block, Order};

    #[test]
    fn diagnostic_matches_production_storage_decision_and_payload_length() {
        let unseen: Vec<u8> = (0..128).collect();
        for data in [&b""[..], &b"x"[..], &b"abracadabra".repeat(10000), &unseen] {
            let m = measure(data);
            assert_eq!(m.packed, encode_block(Order::Ppm, data).map(|p| p.len()));
            if m.packed.is_some() {
                assert_eq!(m.modeled, data.len());
            }
        }
    }
}
