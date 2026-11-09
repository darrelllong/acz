//! `bench compare FILE...`: code each file in 16 MiB blocks, each with a
//! fresh model, with v9 and with trie v4; check that the code strings are
//! identical and that v4 decodes its own.
//!
//! `bench MODEL FILE...`: time encoding and decoding each file with one
//! model (`v9`, `tree`, `tree2` .. `tree4` on the bit-wise coder, `tree5`
//! on the range coder), and check the round trip.
use std::time::Instant;

use ppm_tree::{ppm, ppm2, ppm3, ppm4, ppm5, rc, v9};

const BLOCK: usize = 1 << 24;

/// Encoding and decoding one block with the models on the `arith` coder.
macro_rules! arith_coded {
    ($m:path) => {{
        use $m as m;
        fn enc(block: &[u8]) -> (Vec<u8>, u32) {
            let mut model = Box::new(m::Ppm::new());
            let mut e = arith::Encoder::new(Vec::new());
            for &b in block {
                model.encode(&mut e, b).unwrap();
            }
            (e.finish().unwrap(), model.restarts())
        }
        fn dec(code: &[u8], n: usize) -> Vec<u8> {
            let mut model = Box::new(m::Ppm::new());
            let mut d = arith::Decoder::new(code).unwrap();
            (0..n).map(|_| model.decode(&mut d).unwrap()).collect()
        }
        (enc as fn(&[u8]) -> (Vec<u8>, u32), dec as fn(&[u8], usize) -> Vec<u8>)
    }};
}

fn enc5(block: &[u8]) -> (Vec<u8>, u32) {
    let mut model = Box::new(ppm5::Ppm::new());
    let mut e = rc::Encoder::new(Vec::new());
    for &b in block {
        model.encode(&mut e, b).unwrap();
    }
    (e.finish().unwrap(), model.restarts())
}

fn dec5(code: &[u8], n: usize) -> Vec<u8> {
    let mut model = Box::new(ppm5::Ppm::new());
    let mut d = rc::Decoder::new(code).unwrap();
    (0..n).map(|_| model.decode(&mut d).unwrap()).collect()
}

type Codec = (fn(&[u8]) -> (Vec<u8>, u32), fn(&[u8], usize) -> Vec<u8>);

fn codec(name: &str) -> Codec {
    match name {
        "v9" => arith_coded!(v9),
        "tree" => arith_coded!(ppm),
        "tree2" => arith_coded!(ppm2),
        "tree3" => arith_coded!(ppm3),
        "tree4" => arith_coded!(ppm4),
        "tree5" => (enc5, dec5),
        _ => panic!("unknown model {name}"),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (mode, files) = args.split_first().expect("usage: bench compare|MODEL FILE...");
    if mode == "compare" {
        let ((e9, _), (e4, d4)) = (codec("v9"), codec("tree4"));
        println!("file\tblock\tbytes\tv9_bytes\ttree4_bytes\tv9_restarts\ttree4_restarts\tidentical\ttree4_roundtrip");
        for path in files {
            let data = std::fs::read(path).unwrap();
            let name = path.rsplit('/').next().unwrap();
            for (k, block) in data.chunks(BLOCK).enumerate() {
                let ((a, ra), (b, rb)) = (e9(block), e4(block));
                let back = d4(&b, block.len()) == block;
                println!("{name}\t{k}\t{}\t{}\t{}\t{ra}\t{rb}\t{}\t{back}", block.len(), a.len(), b.len(), a == b);
            }
        }
        return;
    }
    let (enc, dec) = codec(mode);
    println!("file\tmodel\tbytes\tcode_bytes\trestarts\tencode_s\tdecode_s");
    for path in files {
        let data = std::fs::read(path).unwrap();
        let name = path.rsplit('/').next().unwrap();
        let (mut bytes, mut restarts, mut te, mut td) = (0, 0, 0.0, 0.0);
        for block in data.chunks(BLOCK) {
            let t = Instant::now();
            let (code, r) = enc(block);
            te += t.elapsed().as_secs_f64();
            let t = Instant::now();
            let back = dec(&code, block.len());
            td += t.elapsed().as_secs_f64();
            assert!(back == block, "{name}: round trip failed");
            bytes += code.len();
            restarts += r;
        }
        println!("{name}\t{mode}\t{}\t{bytes}\t{restarts}\t{te:.3}\t{td:.3}", data.len());
    }
}
