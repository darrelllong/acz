// Copyright (c) 2026 Darrell D. E. Long. All rights reserved.
// Use and redistribution are subject to the BSD 3-clause license, with a
// dedication clause, in the LICENSE file at the root of this repository.
//
// Dedicated to the memory of my colleague Glen G. Langdon

//! A binary range coder with byte-wise renormalisation.
//!
//! Like [`crate::Encoder`], this splits an interval in proportion to each
//! event's probability. Here each event is binary and normalisation shifts
//! a whole byte at a time (base 256), as advocated by Schindler (1998),
//! Moffat–Neal–Witten (1998, §4.2) and Said (2004,
//! §2.3.1). Byte-at-a-time output removes the per-bit loop of the classic
//! coder, which matters when every event is a binary decision.
//!
//! **Carry handling.** Langdon's carry-over problem appears here as a carry
//! out of the 32-bit `low` register. The encoder keeps the most recent
//! output byte in `cache` together with a count of following `0xFF` bytes;
//! a carry increments `cache` and turns the `0xFF`s into `0x00`s, after
//! which they are written. No byte is ever revised after it is written.
//!
//! **Precision.** Before each event, `range` is in `[2²⁴, 2³²)`. The split
//! for a 16-bit probability is computed exactly as `⌊range · p₀ / 2¹⁶⌋` with a 64-bit
//! product, so both sub-intervals are non-empty for every `p₀ ∈ [1, 2¹⁶)`.
//!
//! **Termination.** [`RangeEncoder::finish`] flushes five bytes; the decoder
//! reads five bytes up front and one per renormalisation, so it consumes
//! exactly the bytes the encoder wrote (given the same event sequence).
//!
//! **References.**
//! M. Schindler, *A Fast Renormalisation for Arithmetic Coding*, Proc. Data
//! Compression Conference, 1998, p. 572.
//! A. Moffat, R. M. Neal and I. H. Witten, *Arithmetic Coding Revisited*,
//! ACM Trans. Inf. Syst. 16(3), 1998, pp. 256–294.
//! A. Said, *Introduction to Arithmetic Coding — Theory and Practice*,
//! HP Laboratories Technical Report HPL-2004-76, 2004.

use std::io::{self, Write};

use crate::model::{BitModel, BIT_PRECISION};

const TOP: u32 = 1 << 24;

/// Byte-wise binary range encoder.
///
/// Errors are final, as for [`crate::Encoder`]: after an I/O error every
/// later call fails.
pub struct RangeEncoder<W: Write> {
    low: u64,
    range: u32,
    cache: u8,
    /// Number of bytes pending output: `cache` plus any `0xFF` bytes.
    pending: u64,
    out: W,
    poisoned: bool,
}

impl<W: Write> RangeEncoder<W> {
    /// Create an encoder writing to `sink` (buffer it if unbuffered).
    pub fn new(sink: W) -> Self {
        RangeEncoder { low: 0, range: u32::MAX, cache: 0, pending: 1, out: sink, poisoned: false }
    }

    /// Encode `bit`, where `p0` is the probability of a 0, as a fraction
    /// of 2¹⁶.
    ///
    /// Zero takes the lower sub-interval; one advances `low` to the split
    /// and takes the upper one. The 64-bit product preserves all 16 bits
    /// of probability precision. Shifting whenever `range < TOP` restores
    /// enough width for both outcomes of the next event to be encodable.
    ///
    /// # Panics
    ///
    /// Panics unless `1 <= p0 <= 65535`.
    #[inline]
    pub fn encode(&mut self, bit: bool, p0: u32) -> io::Result<()> {
        assert!(p0 > 0 && p0 < (1 << BIT_PRECISION), "p0 must be in 1..65536");
        if self.poisoned {
            return Err(crate::coder::poisoned_error());
        }
        let bound = ((u64::from(self.range) * u64::from(p0)) >> BIT_PRECISION) as u32;
        if bit {
            self.low += u64::from(bound);
            self.range -= bound;
        } else {
            self.range = bound;
        }
        while self.range < TOP {
            self.range <<= 8;
            if let Err(e) = self.shift_low() {
                self.poisoned = true;
                return Err(e);
            }
        }
        Ok(())
    }

    /// Encode with an adaptive [`BitModel`], then let it adapt.
    #[inline]
    pub fn encode_bit<const S: u8>(&mut self, model: &mut BitModel<S>, bit: bool) -> io::Result<()> {
        self.encode(bit, model.split())?;
        model.update(bit);
        Ok(())
    }

    /// Encode with an explicit probability of a 1 (`p1 / 2¹⁶`, non-zero).
    #[inline]
    pub fn encode_bit_p(&mut self, bit: bool, p1: u16) -> io::Result<()> {
        assert!(p1 > 0, "p1 must be non-zero");
        self.encode(bit, (1u32 << BIT_PRECISION) - u32::from(p1))
    }

    /// Encode `nbits` low bits of `value` (most significant first) with
    /// probability ½ each.
    ///
    /// # Panics
    ///
    /// Panics if `nbits > 32`.
    #[inline]
    pub fn encode_direct(&mut self, value: u32, nbits: u32) -> io::Result<()> {
        assert!(nbits <= 32, "at most 32 direct bits");
        for i in (0..nbits).rev() {
            self.encode((value >> i) & 1 != 0, 1 << (BIT_PRECISION - 1))?;
        }
        Ok(())
    }

    /// Shift one byte out of `low`, deferring output if a carry could
    /// still change it. `cache` precedes `pending - 1` bytes of 0xff.
    /// When the next byte is below 0xff, or a carry has already arrived,
    /// that earlier run can be resolved: add the carry to `cache`, then
    /// write the run as 0xff (no carry) or 0x00 (carry). Otherwise extend
    /// the pending run. The new high byte becomes the next cached byte;
    /// only the remaining low 24 bits are shifted into the next position.
    fn shift_low(&mut self) -> io::Result<()> {
        if self.low < 0xFF00_0000 || self.low >= 1 << 32 {
            let carry = (self.low >> 32) as u8;
            let mut byte = self.cache;
            while self.pending > 0 {
                self.out.write_all(&[byte.wrapping_add(carry)])?;
                byte = 0xFF;
                self.pending -= 1;
            }
            self.cache = ((self.low >> 24) & 0xFF) as u8;
        }
        self.pending += 1;
        self.low = (self.low & 0x00FF_FFFF) << 8;
        Ok(())
    }

    /// Write the final low endpoint and return the sink. Five shifts
    /// account for the cached byte and the four-byte endpoint, matching
    /// the decoder's five-byte initial read. Together with one read per
    /// normalisation, this lets a known event count delimit the payload.
    pub fn finish(mut self) -> io::Result<W> {
        if self.poisoned {
            return Err(crate::coder::poisoned_error());
        }
        for _ in 0..5 {
            self.shift_low()?;
        }
        self.out.flush()?;
        Ok(self.out)
    }
}

/// Byte-wise binary range decoder over an in-memory code string.
///
/// Reading past the end of the slice does not panic: zero bytes are
/// supplied and the overrun is reported by [`RangeDecoder::finish`].
pub struct RangeDecoder<'a> {
    code: u32,
    range: u32,
    data: &'a [u8],
    pos: usize,
    overrun: bool,
}

impl<'a> RangeDecoder<'a> {
    /// Start decoding `data`. Returns an error if the first byte (always
    /// zero in a valid stream) is not zero or the data is shorter than
    /// five bytes.
    pub fn new(data: &'a [u8]) -> io::Result<Self> {
        if data.len() < 5 || data[0] != 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid range-coded stream"));
        }
        let code = u32::from_be_bytes([data[1], data[2], data[3], data[4]]);
        Ok(RangeDecoder { code, range: u32::MAX, data, pos: 5, overrun: false })
    }

    #[inline]
    fn next_byte(&mut self) -> u8 {
        match self.data.get(self.pos) {
            Some(&b) => {
                self.pos += 1;
                b
            }
            None => {
                self.overrun = true;
                0
            }
        }
    }

    /// Decode a bit whose probability of being 0 is `p0 / 2¹⁶`.
    ///
    /// # Panics
    ///
    /// Panics unless `1 <= p0 <= 65535`.
    #[inline]
    pub fn decode(&mut self, p0: u32) -> bool {
        assert!(p0 > 0 && p0 < (1 << BIT_PRECISION), "p0 must be in 1..65536");
        self.decode_valid(p0)
    }

    /// `decode` for a `p0` already known to be in range (from a
    /// [`BitModel`], whose probabilities never reach 0 or 1).
    ///
    /// `code` is the code value relative to the interval's low endpoint.
    /// Comparing it with the encoder's split selects the same half; for
    /// a one, subtracting the split moves the origin to the new endpoint.
    /// Normalisation shifts both `range` and this relative value, reading
    /// exactly one byte for each shift performed by the encoder.
    #[inline]
    fn decode_valid(&mut self, p0: u32) -> bool {
        debug_assert!(p0 > 0 && p0 < (1 << BIT_PRECISION));
        let bound = ((u64::from(self.range) * u64::from(p0)) >> BIT_PRECISION) as u32;
        let bit = self.code >= bound;
        if bit {
            self.code -= bound;
            self.range -= bound;
        } else {
            self.range = bound;
        }
        while self.range < TOP {
            self.range <<= 8;
            self.code = (self.code << 8) | u32::from(self.next_byte());
        }
        bit
    }

    /// Decode with an adaptive [`BitModel`], then let it adapt.
    #[inline]
    pub fn decode_bit<const S: u8>(&mut self, model: &mut BitModel<S>) -> bool {
        let bit = self.decode_valid(model.split());
        model.update(bit);
        bit
    }

    /// Decode with an explicit probability of a 1 (`p1 / 2¹⁶`, non-zero).
    #[inline]
    pub fn decode_bit_p(&mut self, p1: u16) -> bool {
        assert!(p1 > 0, "p1 must be non-zero");
        self.decode((1u32 << BIT_PRECISION) - u32::from(p1))
    }

    /// Decode `nbits` equiprobable bits (most significant first).
    ///
    /// # Panics
    ///
    /// Panics if `nbits > 32`.
    #[inline]
    pub fn decode_direct(&mut self, nbits: u32) -> u32 {
        assert!(nbits <= 32, "at most 32 direct bits");
        let mut v = 0;
        for _ in 0..nbits {
            v = (v << 1) | u32::from(self.decode_valid(1 << (BIT_PRECISION - 1)));
        }
        v
    }

    /// Finish: succeeds only if the decoder consumed exactly all of the
    /// input, did not run past its end, and the stream ends canonically.
    ///
    /// `code` tracks the difference between the code value and the bottom
    /// of the interval (MNW's `D = V − L`). [`RangeEncoder::finish`] writes
    /// the bottom of the final interval exactly, so a well-formed stream
    /// leaves `code == 0`. Any other value means the bytes are not what
    /// this encoder produces for this event sequence, even if they decode.
    pub fn finish(self) -> io::Result<()> {
        if self.overrun {
            Err(io::Error::new(io::ErrorKind::UnexpectedEof, "range-coded stream is truncated"))
        } else if self.pos != self.data.len() {
            Err(io::Error::new(io::ErrorKind::InvalidData, "trailing bytes after range-coded stream"))
        } else if self.code != 0 {
            Err(io::Error::new(io::ErrorKind::InvalidData, "range-coded stream does not end canonically"))
        } else {
            Ok(())
        }
    }
}
