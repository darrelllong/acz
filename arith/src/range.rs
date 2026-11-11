// Copyright (c) 2026 Darrell D. E. Long. All rights reserved.
// Use and redistribution are subject to the BSD 3-clause license, with a
// dedication clause, in the LICENSE file at the root of this repository.
//
// Dedicated to the memory of my colleague Glen G. Langdon

//! A binary range coder with byte-wise renormalization.
//!
//! Like [`crate::Encoder`], it splits an interval in proportion to each event's
//! probability. Every event is binary and renormalization shifts a whole byte,
//! after Schindler (1998), Moffat, Neal and Witten (1998, §4.2) and Said (2004,
//! §2.3.1). That removes the bit-wise coder's loop over output bits, which
//! matters when every event is a binary decision.
//!
//! # Carry
//!
//! Langdon's carry-over problem appears as a carry out of the 32-bit `low`.
//! [`crate::carry`] holds back the bytes a carry could change, so no byte
//! changes after it is written.
//!
//! # Precision
//!
//! Before each event `range` is in `[2²⁴, 2³²)`. The split for a 16-bit
//! probability is `⌊range · p₀ / 2¹⁶⌋`, computed with a 64-bit product, so both
//! parts are non-empty for every `p₀` in `[1, 2¹⁶)`.
//!
//! # Termination
//!
//! [`RangeEncoder::finish`] writes five bytes. The decoder reads five bytes to
//! start and one per renormalization, so for the same events it reads the bytes
//! written and no more.
//!
//! # References
//!
//! M. Schindler, *A Fast Renormalisation for Arithmetic Coding*, Proc. Data
//! Compression Conference, 1998, p. 572.
//!
//! A. Moffat, R. M. Neal and I. H. Witten, *Arithmetic Coding Revisited*, ACM
//! Trans. Inf. Syst. 16(3), 1998, pp. 256–294.
//!
//! A. Said, *Introduction to Arithmetic Coding — Theory and Practice*, HP
//! Laboratories Technical Report HPL-2004-76, 2004.

use std::io::{self, Write};

use crate::carry::Carry;
use crate::model::{BitModel, BIT_PRECISION};

const TOP: u32 = 1 << 24;

/// A byte-wise binary range encoder. After an I/O error every later call fails.
pub struct RangeEncoder<W: Write> {
    /// The low end of the interval: 32 bits, and above them a carry.
    low: u64,
    range: u32,
    carry: Carry,
    out: W,
    poisoned: bool,
}

impl<W: Write> RangeEncoder<W> {
    /// Returns an encoder writing to `sink`. The encoder writes a byte at a
    /// time, so `sink` should be buffered.
    pub fn new(sink: W) -> Self {
        RangeEncoder { low: 0, range: u32::MAX, carry: Carry::new(), out: sink, poisoned: false }
    }

    /// Codes `bit`, where a 0 has probability `p0 / 2¹⁶`. A 0 takes the lower
    /// part of the range. A 1 moves `low` to the split and takes the upper
    /// part.
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

    /// Codes `bit` with an adaptive [`BitModel`], then lets the model adapt.
    #[inline]
    pub fn encode_bit<const S: u8>(&mut self, model: &mut BitModel<S>, bit: bool) -> io::Result<()> {
        self.encode(bit, model.split())?;
        model.update(bit);
        Ok(())
    }

    /// Codes `bit`, where a 1 has probability `p1 / 2¹⁶`.
    ///
    /// # Panics
    ///
    /// Panics if `p1 == 0`.
    #[inline]
    pub fn encode_bit_p(&mut self, bit: bool, p1: u16) -> io::Result<()> {
        assert!(p1 > 0, "p1 must be non-zero");
        self.encode(bit, (1u32 << BIT_PRECISION) - u32::from(p1))
    }

    /// Codes the low `nbits` bits of `value`, most significant first, at
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

    /// Moves the top byte of `low` out, with the carry above it if there is
    /// one.
    #[inline]
    fn shift_low(&mut self) -> io::Result<()> {
        self.carry.push(&mut self.out, (self.low >> 24) as u8, self.low >> 32 != 0)?;
        self.low = (self.low & 0x00FF_FFFF) << 8;
        Ok(())
    }

    /// Writes the final `low` and returns the sink: five bytes, the held-back
    /// one and the four of `low`.
    pub fn finish(mut self) -> io::Result<W> {
        if self.poisoned {
            return Err(crate::coder::poisoned_error());
        }
        for _ in 0..4 {
            self.shift_low()?;
        }
        self.carry.flush(&mut self.out)?;
        self.out.flush()?;
        Ok(self.out)
    }
}

/// A byte-wise binary range decoder over a code string in memory. Reading past
/// the end supplies zeros, and [`RangeDecoder::finish`] reports the overrun.
pub struct RangeDecoder<'a> {
    code: u32,
    range: u32,
    data: &'a [u8],
    pos: usize,
    overrun: bool,
}

impl<'a> RangeDecoder<'a> {
    /// Returns a decoder of `data`, or an error if `data` is shorter than five
    /// bytes or does not begin with the zero byte that begins every stream.
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

    /// Decodes a bit, where a 0 has probability `p0 / 2¹⁶`.
    ///
    /// # Panics
    ///
    /// Panics unless `1 <= p0 <= 65535`.
    #[inline]
    pub fn decode(&mut self, p0: u32) -> bool {
        assert!(p0 > 0 && p0 < (1 << BIT_PRECISION), "p0 must be in 1..65536");
        self.decode_valid(p0)
    }

    /// Decodes a bit for a `p0` known to be in range. `code` is the code value
    /// relative to the low end of the interval. It is compared with the
    /// encoder's split. For a 1 the split is subtracted, which moves the origin
    /// to the new low end. Each shift reads one byte, as the encoder wrote one.
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

    /// Decodes a bit with an adaptive [`BitModel`], then lets the model adapt.
    #[inline]
    pub fn decode_bit<const S: u8>(&mut self, model: &mut BitModel<S>) -> bool {
        let bit = self.decode_valid(model.split());
        model.update(bit);
        bit
    }

    /// Decodes a bit, where a 1 has probability `p1 / 2¹⁶`.
    ///
    /// # Panics
    ///
    /// Panics if `p1 == 0`.
    #[inline]
    pub fn decode_bit_p(&mut self, p1: u16) -> bool {
        assert!(p1 > 0, "p1 must be non-zero");
        self.decode((1u32 << BIT_PRECISION) - u32::from(p1))
    }

    /// Decodes `nbits` bits of probability ½ each, most significant first.
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

    /// Ends decoding. It succeeds only if the decoder read all of the input,
    /// did not read past its end, and is left with `code == 0`.
    ///
    /// `code` is the code value less the low end of the interval.
    /// [`RangeEncoder::finish`] writes the low end of the final interval, so
    /// the encoder's stream leaves it at 0. Any other value means the bytes are
    /// not the ones the encoder writes for these events, even if they decode to
    /// them.
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
