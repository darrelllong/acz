// Copyright (c) 2026 Darrell D. E. Long. All rights reserved.
// Use and redistribution are subject to the BSD 3-clause license, with a
// dedication clause, in the LICENSE file at the root of this repository.
//
// Dedicated to the memory of my colleague Glen G. Langdon

//! A multi-symbol range coder with byte-wise renormalization.
//!
//! An event is an interval `[l, h)` of a total `t`, as for
//! [`crate::Encoder`], or a binary decision with a 16-bit probability.
//! Following Moffat, Neal and Witten (1998, §4.4), the encoder divides
//! once per interval: `r = ⌊R / t⌋`, the low end moves up by `r·l`, and the
//! range becomes `r·(h − l)`, or, for the last event of the total
//! (`h = t`), all that is left above `r·l`, so that no part of the range
//! goes unused. The decoder keeps the code value relative to the low end
//! (their `D = V − L`), divides once for `r` and once to find the target.
//! A binary decision splits the range with a shift and one product.
//!
//! Output is a byte at a time, as in [`crate::RangeEncoder`] (Schindler,
//! 1998): whenever the range falls below 2⁴⁸ its top byte is settled except
//! for a carry, which is Langdon's carry-over problem. The last byte
//! written is held back in `cache`, with a count of the `0xFF` bytes after
//! it; a carry out of the 56-bit `low` increments `cache` and turns those
//! into `0x00`. No byte is changed after it is written. Against the
//! bit-wise [`crate::Encoder`], this saves the per-bit loop: one division
//! and a renormalization by whole bytes per event.
//!
//! **Precision.** Before each event the range is in `[2⁴⁸, 2⁵⁶)`. With
//! totals of at most [`MULTI_MAX_TOTAL`] = 2²⁰, `r` is at least 2²⁸, so
//! rounding it down gives up less than one part in 2²⁸ of the range. A
//! binary decision splits at `⌊R / 2¹⁶⌋·(2¹⁶ − p₁)`, which leaves both parts
//! non-empty for every `p₁ ∈ [1, 2¹⁶)`.
//!
//! **Termination.** [`MultiRangeEncoder::finish`] writes out all of `low`:
//! eight bytes with the held-back one. The first byte of a stream is
//! always 0. The decoder reads eight bytes to start and one per
//! renormalization, so it reads exactly what was written, and the relative
//! code value ends at 0.
//!
//! **Canonical decoding.** The relative code value of a valid stream is
//! always below the range. That holds at the start if the first eight bytes
//! say so, and every step keeps it: a symbol below the last leaves
//! `code − r·l < r·(h − l)`; the last symbol, which a corrupt target is
//! clamped to, takes all that is left of the range; a binary decision
//! keeps the code value in the part it falls in; and renormalizing maps
//! `code < range < 2⁴⁸` to `256·code + b < 256·range`. So the 56-bit mask
//! never discards a bit, the code value tracks the stream exactly, and
//! ending at 0 after exactly the bytes written leaves one stream per event
//! sequence. [`MultiRangeDecoder::new`] therefore rejects a stream that
//! starts with the code value at or above the range: without that check, a
//! stream starting `00 ff ff ff ff ff ff ff` decodes like one starting
//! `00 ff ff ff ff ff ff fe` once the mask has dropped the excess.
//!
//! **References.**
//! A. Moffat, R. M. Neal and I. H. Witten, *Arithmetic Coding Revisited*,
//! ACM Trans. Inf. Syst. 16(3), 1998, pp. 256–294.
//! M. Schindler, *A Fast Renormalisation for Arithmetic Coding*, Proc. Data
//! Compression Conference, 1998, p. 572.
//! G. G. Langdon, *An Introduction to Arithmetic Coding*, IBM J. Res.
//! Develop. 28(2), 1984, pp. 135–149 (carry-over).

use std::io::{self, Write};

/// Renormalize when the range falls below this.
const TOP: u64 = 1 << 48;
/// `low` holds 56 bits; bit 56 is a carry.
const LOW_BITS: u32 = 56;
const LOW_MASK: u64 = (1 << LOW_BITS) - 1;

/// The largest total an interval event may have.
pub const MULTI_MAX_TOTAL: u32 = 1 << 20;

fn poisoned_error() -> io::Error {
    io::Error::other("range encoder unusable after an earlier I/O error")
}

/// Byte-wise multi-symbol range encoder.
///
/// Errors are final, as for [`crate::Encoder`]: after an I/O error every
/// later call fails.
pub struct MultiRangeEncoder<W: Write> {
    low: u64,
    range: u64,
    cache: u8,
    /// Bytes pending output: `cache` and the `0xFF` bytes after it.
    pending: u64,
    out: W,
    poisoned: bool,
}

impl<W: Write> MultiRangeEncoder<W> {
    /// An encoder writing to `sink` (buffer it if unbuffered).
    pub fn new(sink: W) -> Self {
        MultiRangeEncoder { low: 0, range: LOW_MASK, cache: 0, pending: 1, out: sink, poisoned: false }
    }

    /// Code the interval `[cum_low, cum_high)` of `[0, total)`.
    ///
    /// # Panics
    ///
    /// Panics unless `cum_low < cum_high <= total <= MULTI_MAX_TOTAL`.
    #[inline]
    pub fn encode(&mut self, cum_low: u32, cum_high: u32, total: u32) -> io::Result<()> {
        assert!(
            cum_low < cum_high && cum_high <= total && total <= MULTI_MAX_TOTAL,
            "invalid coding interval [{cum_low}, {cum_high}) of {total} (MULTI_MAX_TOTAL = {MULTI_MAX_TOTAL})"
        );
        if self.poisoned {
            return Err(poisoned_error());
        }
        let r = self.range / u64::from(total);
        let below = r * u64::from(cum_low);
        self.low += below;
        self.range = if cum_high < total { r * u64::from(cum_high - cum_low) } else { self.range - below };
        self.normalize()
    }

    /// Code a binary event whose 1 has probability `p1 / 2¹⁶`: the 0 takes
    /// the lower part of the range, the 1 the upper, as in
    /// [`crate::Encoder::encode_bit_p`].
    ///
    /// # Panics
    ///
    /// Panics if `p1 == 0`.
    #[inline]
    pub fn encode_bit_p(&mut self, bit: bool, p1: u16) -> io::Result<()> {
        assert!(p1 > 0, "p1 must be non-zero");
        if self.poisoned {
            return Err(poisoned_error());
        }
        let split = (self.range >> 16) * u64::from((1u32 << 16) - u32::from(p1));
        if bit {
            self.low += split;
            self.range -= split;
        } else {
            self.range = split;
        }
        self.normalize()
    }

    #[inline]
    fn normalize(&mut self) -> io::Result<()> {
        while self.range < TOP {
            self.range <<= 8;
            if let Err(e) = self.shift_low() {
                self.poisoned = true;
                return Err(e);
            }
        }
        Ok(())
    }

    /// Move the top byte of `low` out: write the held-back byte and the
    /// `0xFF`s after it once no carry can reach them, else count one more
    /// `0xFF`.
    fn shift_low(&mut self) -> io::Result<()> {
        if self.low < 0xFF << (LOW_BITS - 8) || self.low > LOW_MASK {
            let carry = (self.low >> LOW_BITS) as u8;
            let mut byte = self.cache;
            while self.pending > 0 {
                self.out.write_all(&[byte.wrapping_add(carry)])?;
                byte = 0xFF;
                self.pending -= 1;
            }
            self.cache = (self.low >> (LOW_BITS - 8)) as u8;
        }
        self.pending += 1;
        self.low = (self.low << 8) & LOW_MASK;
        Ok(())
    }

    /// Write out the rest of the code string and return the sink.
    pub fn finish(mut self) -> io::Result<W> {
        if self.poisoned {
            return Err(poisoned_error());
        }
        for _ in 0..8 {
            self.shift_low()?;
        }
        self.out.flush()?;
        Ok(self.out)
    }
}

/// Byte-wise multi-symbol range decoder, reading a whole code string.
pub struct MultiRangeDecoder<'a> {
    /// The code value less the low end of the interval.
    code: u64,
    range: u64,
    /// `⌊range / total⌋` from the last [`MultiRangeDecoder::target`], for
    /// [`MultiRangeDecoder::consume`].
    r: u64,
    data: &'a [u8],
    pos: usize,
    /// Whether the decoder has read past the end of `data` (as zeros).
    overrun: bool,
}

impl<'a> MultiRangeDecoder<'a> {
    /// A decoder of the code string `data`.
    ///
    /// Fails if `data` cannot be the start of a code string the encoder
    /// writes.
    pub fn new(data: &'a [u8]) -> io::Result<Self> {
        if data.len() < 8 || data[0] != 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid range-coded stream"));
        }
        let code = data[1..8].iter().fold(0, |c, &b| (c << 8) | u64::from(b));
        if code >= LOW_MASK {
            // Outside the initial range [0, 2⁵⁶ − 1): no encoder writes this.
            return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid range-coded stream"));
        }
        Ok(MultiRangeDecoder { code, range: LOW_MASK, r: 1, data, pos: 8, overrun: false })
    }

    /// Where the code value falls in `[0, total)`: the `x` of the event
    /// `[cum_low, cum_high)` with `cum_low <= x < cum_high`.
    /// [`MultiRangeDecoder::consume`] must follow, with the same `total`.
    ///
    /// # Panics
    ///
    /// Panics unless `0 < total <= MULTI_MAX_TOTAL`.
    #[inline]
    pub fn target(&mut self, total: u32) -> u32 {
        assert!(total > 0 && total <= MULTI_MAX_TOTAL, "invalid total {total}");
        self.r = self.range / u64::from(total);
        // A corrupt stream can put the code value past the last event.
        (self.code / self.r).min(u64::from(total - 1)) as u32
    }

    /// Remove `[cum_low, cum_high)` of `total`, the event holding the last
    /// target.
    #[inline]
    pub fn consume(&mut self, cum_low: u32, cum_high: u32, total: u32) {
        debug_assert!(cum_low < cum_high && cum_high <= total);
        let below = self.r * u64::from(cum_low);
        self.code = self.code.wrapping_sub(below);
        self.range = if cum_high < total { self.r * u64::from(cum_high - cum_low) } else { self.range - below };
        self.normalize();
    }

    /// Decode a binary event coded by [`MultiRangeEncoder::encode_bit_p`].
    ///
    /// # Panics
    ///
    /// Panics if `p1 == 0`.
    #[inline]
    pub fn decode_bit_p(&mut self, p1: u16) -> bool {
        assert!(p1 > 0, "p1 must be non-zero");
        let split = (self.range >> 16) * u64::from((1u32 << 16) - u32::from(p1));
        let bit = self.code >= split;
        if bit {
            self.code -= split;
            self.range -= split;
        } else {
            self.range = split;
        }
        self.normalize();
        bit
    }

    #[inline]
    fn normalize(&mut self) {
        debug_assert!(self.code < self.range, "the code value left the range");
        while self.range < TOP {
            self.range <<= 8;
            let b = match self.data.get(self.pos) {
                Some(&b) => {
                    self.pos += 1;
                    b
                }
                None => {
                    self.overrun = true;
                    0
                }
            };
            self.code = ((self.code << 8) | u64::from(b)) & LOW_MASK;
        }
    }

    /// Whether the stream ended exactly where the encoder finished it: every
    /// byte read, none past the end, and the code value at the low end.
    pub fn is_canonical_end(&self) -> bool {
        !self.overrun && self.pos == self.data.len() && self.code == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Events of every shape, including long runs of near-certain ones
    /// (many pending `0xFF`s) and totals at the limit.
    fn events() -> Vec<(u32, u32, u32)> {
        let mut s = 0x2545F4914F6CDD1Du64;
        let mut next = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        let mut v = Vec::new();
        for k in 0..200_000 {
            let t = match k % 5 {
                0 => MULTI_MAX_TOTAL,
                1 => 2,
                _ => 1 + (next() % u64::from(MULTI_MAX_TOTAL)) as u32,
            };
            let (l, h) = if k % 3 == 0 {
                // Nearly certain: the whole total but one.
                (0, t.max(2) - 1)
            } else {
                let a = (next() % u64::from(t)) as u32;
                let b = (next() % u64::from(t)) as u32;
                (a.min(b), a.max(b) + 1)
            };
            let t = t.max(h);
            v.push((l, h, t));
        }
        v
    }

    /// Binary probabilities across the whole 16-bit scale.
    fn p1(k: usize) -> u16 {
        match k % 4 {
            0 => 1,
            1 => u16::MAX,
            _ => 1 + (k as u64 * 40503 % 65535) as u16,
        }
    }

    #[test]
    fn roundtrips_events_and_binary_decisions() {
        let ev = events();
        let mut enc = MultiRangeEncoder::new(Vec::new());
        for (k, &(l, h, t)) in ev.iter().enumerate() {
            enc.encode(l, h, t).unwrap();
            enc.encode_bit_p(k % 7 == 0, p1(k)).unwrap();
        }
        let code = enc.finish().unwrap();
        let mut dec = MultiRangeDecoder::new(&code).unwrap();
        for (k, &(l, h, t)) in ev.iter().enumerate() {
            let x = dec.target(t);
            assert!(l <= x && x < h, "event {k}");
            dec.consume(l, h, t);
            assert_eq!(dec.decode_bit_p(p1(k)), k % 7 == 0, "binary {k}");
        }
        assert!(dec.is_canonical_end());
    }

    /// A binary event costs what its probability says: a million decisions
    /// at p₁ = 1/16, a sixteenth of them ones, take within 0.1% of their
    /// entropy.
    #[test]
    fn binary_decisions_cost_their_entropy() {
        let n = 1_000_000;
        let mut enc = MultiRangeEncoder::new(Vec::new());
        for k in 0..n {
            enc.encode_bit_p(k % 16 == 0, 1 << 12).unwrap();
        }
        let bytes = enc.finish().unwrap().len() as f64;
        let (p, q) = (1.0 / 16.0, 15.0 / 16.0);
        let entropy = n as f64 * -(p * f64::log2(p) + q * f64::log2(q)) / 8.0;
        assert!((bytes - entropy).abs() < entropy * 0.001 + 16.0, "{bytes} bytes, entropy {entropy}");
    }

    /// Streams that differ from the encoder's output only where the first
    /// eight bytes put the code value at the top of the range are rejected;
    /// the encoder's are accepted.
    #[test]
    fn only_the_encoders_stream_decodes() {
        let hex = |s: &str| {
            (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect::<Vec<u8>>()
        };
        for (n, canonical, alias) in [
            (8, "00fffffffffffffefeffffffff0200", "00fffffffffffffffeffffffff0200"),
            (9, "00fffffffffffffefffeffffffff0200", "00fffffffffffffffffeffffffff0200"),
            (
                31,
                "00fffffffffffffefffffffffffffffffffffffffffffffffffffffffffffffeffffffff0200",
                "00fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffeffffffff0200",
            ),
        ] {
            let mut enc = MultiRangeEncoder::new(Vec::new());
            for _ in 0..n {
                enc.encode(255, 256, 256).unwrap();
            }
            assert_eq!(enc.finish().unwrap(), hex(canonical), "{n} events");
            let code = hex(canonical);
            let mut dec = MultiRangeDecoder::new(&code).unwrap();
            for _ in 0..n {
                assert_eq!(dec.target(256), 255);
                dec.consume(255, 256, 256);
            }
            assert!(dec.is_canonical_end());
            assert!(MultiRangeDecoder::new(&hex(alias)).is_err(), "{n} events: the alias is accepted");
        }
    }

    /// Every one-byte change, truncation and extension of a stream either
    /// fails to start, decodes other events, or does not end canonically.
    #[test]
    fn no_other_stream_decodes_the_same_events() {
        let events: Vec<(u32, u32, u32)> = (0..400u32)
            .map(|k| match k % 4 {
                0 => (255, 256, 256),
                1 => (0, 1, 3),
                2 => (1, 4095, 4096),
                _ => (k % 7, k % 7 + 1, 7),
            })
            .collect();
        let bits = |k: usize| (k % 5 == 0, p1(k));
        let mut enc = MultiRangeEncoder::new(Vec::new());
        for (k, &(l, h, t)) in events.iter().enumerate() {
            enc.encode(l, h, t).unwrap();
            let (b, p) = bits(k);
            enc.encode_bit_p(b, p).unwrap();
        }
        let code = enc.finish().unwrap();
        let decodes = |data: &[u8]| -> bool {
            let Ok(mut dec) = MultiRangeDecoder::new(data) else { return false };
            for (k, &(l, h, t)) in events.iter().enumerate() {
                let x = dec.target(t);
                if !(l <= x && x < h) {
                    return false;
                }
                dec.consume(l, h, t);
                let (b, p) = bits(k);
                if dec.decode_bit_p(p) != b {
                    return false;
                }
            }
            dec.is_canonical_end()
        };
        assert!(decodes(&code));
        for i in 0..code.len() {
            for b in 0..=255u8 {
                if b != code[i] {
                    let mut other = code.clone();
                    other[i] = b;
                    assert!(!decodes(&other), "byte {i} changed to {b:#04x} is accepted");
                }
            }
        }
        for n in 0..code.len() {
            assert!(!decodes(&code[..n]), "truncated to {n} bytes is accepted");
        }
        assert!(!decodes(&[&code[..], &[0]].concat()), "a trailing byte is accepted");
    }

    #[test]
    fn carries_reach_back_over_runs_of_ff() {
        // The top 1/256 of the range writes a byte of ones; a run of those
        // leaves `0xFF`s pending, and the next event that adds to `low`
        // after a shift can carry across all of them.
        let pick = |k: u64| (k.wrapping_mul(0x9E3779B97F4A7C15) >> 60) < 14;
        let n = 100_000;
        let mut enc = MultiRangeEncoder::new(Vec::new());
        for k in 0..n {
            if pick(k) {
                enc.encode(255, 256, 256).unwrap();
            } else {
                enc.encode(1, 256, 256).unwrap();
            }
        }
        let code = enc.finish().unwrap();
        // Runs of 0xFF and the 0x00s a carry leaves were both written.
        assert!(code.windows(4).any(|w| w == [0xFF; 4]) && code.windows(2).any(|w| w[0] != 0xFF && w[1] == 0));
        let mut dec = MultiRangeDecoder::new(&code).unwrap();
        for k in 0..n {
            let x = dec.target(256);
            if pick(k) {
                assert_eq!(x, 255, "event {k}");
                dec.consume(255, 256, 256);
            } else {
                assert!(x >= 1, "event {k}");
                dec.consume(1, 256, 256);
            }
        }
        assert!(dec.is_canonical_end());
    }

    /// After a sink error, every later call fails.
    #[test]
    fn errors_are_final() {
        struct Full;
        impl Write for Full {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("full"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut enc = MultiRangeEncoder::new(Full);
        let mut failed = false;
        for _ in 0..64 {
            if enc.encode(1, 2, 4096).is_err() {
                failed = true;
                break;
            }
        }
        assert!(failed);
        assert!(enc.encode(0, 1, 2).is_err());
        assert!(enc.encode_bit_p(true, 1).is_err());
        assert!(enc.finish().is_err());
    }
}
