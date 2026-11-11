// Copyright (c) 2026 Darrell D. E. Long. All rights reserved.
// Use and redistribution are subject to the BSD 3-clause license, with a
// dedication clause, in the LICENSE file at the root of this repository.
//
// Dedicated to the memory of my colleague Glen G. Langdon

//! The bit-wise arithmetic coder: interval narrowing, renormalization, pending
//! bits for the carry, and termination.

use std::io::{self, Read, Write};

use crate::model::{BitModel, Model, BIT_PRECISION};

/// Width of the `low` / `high` registers, in bits.
const CODE_BITS: u32 = 32;
/// Largest value representable in a code register.
const TOP: u64 = (1 << CODE_BITS) - 1;
const HALF: u64 = 1 << (CODE_BITS - 1);
const QUARTER: u64 = 1 << (CODE_BITS - 2);
const THREE_QUARTERS: u64 = HALF + QUARTER;

/// The largest `total` an event may have.
///
/// After renormalization the interval is wider than a quarter of the code
/// space, 2³⁰. With `total ≤ 2³⁰` every symbol of non-zero frequency gets a
/// non-empty sub-interval, and every product of a width and a cumulative count
/// is at most 2⁶².
pub const MAX_TOTAL: u32 = 1 << (CODE_BITS - 2);

#[inline]
fn check_interval(cum_low: u32, cum_high: u32, total: u32) {
    assert!(
        cum_low < cum_high && cum_high <= total && total <= MAX_TOTAL,
        "invalid coding interval [{cum_low}, {cum_high}) of {total} (MAX_TOTAL = {MAX_TOTAL})"
    );
}

/// Narrows `[low, high]` to the sub-interval `[cum_low, cum_high)` of
/// `[0, total)`. Both ends are computed from the old `low`, so `high` is
/// assigned first. The interval is closed, hence the `+ 1` in the width and the
/// `− 1` at the upper end.
#[inline]
fn narrow(low: &mut u64, high: &mut u64, cum_low: u32, cum_high: u32, total: u32) {
    let range = *high - *low + 1;
    let total = u64::from(total);
    *high = *low + range * u64::from(cum_high) / total - 1;
    *low += range * u64::from(cum_low) / total;
}

struct BitWriter<W: Write> {
    inner: W,
    byte: u8,
    nbits: u32,
}

impl<W: Write> BitWriter<W> {
    fn new(inner: W) -> Self {
        BitWriter { inner, byte: 0, nbits: 0 }
    }

    #[inline]
    fn put(&mut self, bit: bool) -> io::Result<()> {
        self.byte = (self.byte << 1) | u8::from(bit);
        self.nbits += 1;
        if self.nbits == 8 {
            self.inner.write_all(&[self.byte])?;
            self.byte = 0;
            self.nbits = 0;
        }
        Ok(())
    }

    /// Pads the last byte with zeros and returns the sink.
    fn finish(mut self) -> io::Result<W> {
        if self.nbits > 0 {
            let b = self.byte << (8 - self.nbits);
            self.inner.write_all(&[b])?;
        }
        self.inner.flush()?;
        Ok(self.inner)
    }
}

/// An arithmetic encoder writing a bit stream to any [`Write`] sink.
///
/// The encoder writes a byte at a time, so an unbuffered sink (a file, a
/// socket, stdout) should be wrapped in a [`std::io::BufWriter`].
/// [`Encoder::finish`] writes the tail of the code string; dropping the encoder
/// without it loses the tail. After an I/O error the state may not match the
/// output, and every later call fails.
pub struct Encoder<W: Write> {
    low: u64,
    high: u64,
    /// Middle-half shifts whose output is deferred. The next resolved
    /// leading bit is followed by this many copies of its complement.
    pending: u64,
    out: BitWriter<W>,
    poisoned: bool,
}

pub(crate) fn poisoned_error() -> io::Error {
    io::Error::other("arithmetic coder is unusable after an earlier error")
}

impl<W: Write> Encoder<W> {
    /// Returns an encoder writing to `sink`.
    pub fn new(sink: W) -> Self {
        Encoder { low: 0, high: TOP, pending: 0, out: BitWriter::new(sink), poisoned: false }
    }

    /// Codes one event, the sub-interval `[cum_low, cum_high)` of `[0, total)`.
    ///
    /// # Panics
    ///
    /// Panics unless `cum_low < cum_high <= total <= MAX_TOTAL`.
    pub fn encode(&mut self, cum_low: u32, cum_high: u32, total: u32) -> io::Result<()> {
        check_interval(cum_low, cum_high, total);
        if self.poisoned {
            return Err(poisoned_error());
        }
        narrow(&mut self.low, &mut self.high, cum_low, cum_high, total);
        self.renormalize()
    }

    /// Codes `symbol` under `model`, then lets the model adapt.
    pub fn encode_symbol<M: Model + ?Sized>(&mut self, model: &mut M, symbol: usize) -> io::Result<()> {
        let (lo, hi) = model.interval(symbol);
        self.encode(lo, hi, model.total())?;
        model.update(symbol);
        Ok(())
    }

    /// Codes a binary event with an adaptive [`BitModel`], then lets the model
    /// adapt.
    pub fn encode_bit<const S: u8>(&mut self, model: &mut BitModel<S>, bit: bool) -> io::Result<()> {
        self.encode_split(model.split(), bit)?;
        model.update(bit);
        Ok(())
    }

    /// Codes a binary event whose probability of being 1 is `p1 / 2^16`, as
    /// supplied by an outside predictor. The decoder must call
    /// [`Decoder::decode_bit_p`] with the same `p1`.
    ///
    /// # Panics
    ///
    /// Panics if `p1 == 0`, which would leave no room for a 1.
    #[inline]
    pub fn encode_bit_p(&mut self, bit: bool, p1: u16) -> io::Result<()> {
        assert!(p1 > 0, "p1 must be non-zero");
        self.encode_split((1u32 << BIT_PRECISION) - u32::from(p1), bit)
    }

    /// Codes a binary event of total 2¹⁶. It equals `encode(0, split, 2^16)` or
    /// `encode(split, 2^16, 2^16)`, with the division done as a shift.
    #[inline]
    fn encode_split(&mut self, split: u32, bit: bool) -> io::Result<()> {
        if self.poisoned {
            return Err(poisoned_error());
        }
        let range = self.high - self.low + 1;
        let mid = self.low + ((range * u64::from(split)) >> BIT_PRECISION);
        if bit {
            self.low = mid;
        } else {
            self.high = mid - 1;
        }
        self.renormalize()
    }

    /// Writes a settled leading bit and the pending bits after it, which are
    /// its complement: a 0 with three pending writes 0111, and a 1 writes 1000.
    #[inline]
    fn emit(&mut self, bit: bool) -> io::Result<()> {
        self.out.put(bit)?;
        while self.pending > 0 {
            self.out.put(!bit)?;
            self.pending -= 1;
        }
        Ok(())
    }

    #[inline]
    fn renormalize(&mut self) -> io::Result<()> {
        let r = self.renormalize_inner();
        if r.is_err() {
            self.poisoned = true;
        }
        r
    }

    /// Restores precision after narrowing (Witten, Neal and Cleary). An
    /// interval within one half has a settled leading bit, which is written and
    /// shifted out. An interval within the middle half has none yet; its second
    /// bit is removed and the decision is deferred in `pending`. Each step
    /// doubles the interval. On return it straddles the midpoint and reaches
    /// outside the middle half, so it is wider than a quarter, as the next
    /// event's rounding needs.
    #[inline]
    fn renormalize_inner(&mut self) -> io::Result<()> {
        loop {
            if self.high < HALF {
                self.emit(false)?;
            } else if self.low >= HALF {
                self.emit(true)?;
                self.low -= HALF;
                self.high -= HALF;
            } else if self.low >= QUARTER && self.high < THREE_QUARTERS {
                self.pending += 1;
                self.low -= QUARTER;
                self.high -= QUARTER;
            } else {
                return Ok(());
            }
            self.low <<= 1;
            // The closed upper end doubles to an odd value.
            self.high = (self.high << 1) | 1;
        }
    }

    /// Ends the code string, flushes, and returns the sink.
    ///
    /// All 32 bits of `low` are written, with any pending bits. `low` lies in
    /// the final interval, and the decoder reads 32 bits to start and one per
    /// shift, so it reads the bits written here and no more. The last byte is
    /// padded with zeros.
    pub fn finish(mut self) -> io::Result<W> {
        if self.poisoned {
            return Err(poisoned_error());
        }
        let first = self.low & HALF != 0;
        self.emit(first)?;
        for i in (0..CODE_BITS - 1).rev() {
            self.out.put((self.low >> i) & 1 != 0)?;
        }
        self.out.finish()
    }

    /// Returns a reference to the sink.
    pub fn get_ref(&self) -> &W {
        &self.out.inner
    }
}

struct BitReader<R: Read> {
    inner: R,
    byte: u8,
    nbits: u32,
}

impl<R: Read> BitReader<R> {
    fn new(inner: R) -> Self {
        BitReader { inner, byte: 0, nbits: 0 }
    }

    #[inline]
    fn get(&mut self) -> io::Result<bool> {
        if self.nbits == 0 {
            let mut buf = [0u8; 1];
            loop {
                match self.inner.read(&mut buf) {
                    Ok(0) => {
                        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "arithmetic code string is truncated"))
                    }
                    Ok(_) => break,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e),
                }
            }
            self.byte = buf[0];
            self.nbits = 8;
        }
        self.nbits -= 1;
        Ok((self.byte >> self.nbits) & 1 != 0)
    }
}

/// An arithmetic decoder reading a bit stream from any [`Read`] source.
///
/// The decoder reads a byte at a time, so an unbuffered source should be
/// wrapped in a [`std::io::BufReader`]. A code string does not mark its own
/// end. Driven through the same events as the encoder, the decoder stops at the
/// end of the code string written by [`Encoder::finish`] and reads nothing
/// beyond it, so the source may continue with other data (see
/// [`Decoder::finish`]).
///
/// Decoding takes two steps, which lets any model drive it. [`Decoder::target`]
/// returns a value in `[0, total)`. The caller finds the symbol whose interval
/// `[cum_low, cum_high)` holds it and passes that interval to
/// [`Decoder::consume`]. [`Decoder::decode_symbol`] does both for a [`Model`].
///
/// After an I/O error, or an interval that does not hold the code value, every
/// later call fails.
pub struct Decoder<R: Read> {
    low: u64,
    high: u64,
    /// Invariant while not poisoned: `low <= value <= high`.
    value: u64,
    input: BitReader<R>,
    poisoned: bool,
}

impl<R: Read> Decoder<R> {
    /// Returns a decoder reading from `source`, after reading the first 32
    /// bits.
    pub fn new(source: R) -> io::Result<Self> {
        let mut input = BitReader::new(source);
        let mut value = 0u64;
        for _ in 0..CODE_BITS {
            value = (value << 1) | u64::from(input.get()?);
        }
        Ok(Decoder { low: 0, high: TOP, value, input, poisoned: false })
    }

    /// Returns the position of the code value in the current interval, scaled
    /// to `[0, total)`. The caller finds the symbol whose interval holds it.
    ///
    /// The result is below `total` even for corrupt input, because
    /// `low <= value <= high`. After an error it is meaningless but still below
    /// `total`, and the next [`Decoder::consume`] reports the error.
    ///
    /// # Panics
    ///
    /// Panics if `total == 0` or `total > MAX_TOTAL`.
    pub fn target(&self, total: u32) -> u32 {
        assert!(total > 0 && total <= MAX_TOTAL, "invalid total {total}");
        // The clamps matter only after an error.
        let range = self.high.wrapping_sub(self.low).wrapping_add(1).max(1);
        let offset = self.value.saturating_sub(self.low).min(range - 1);
        // The inverse of narrow's rounding: ⌈(offset + 1)·T / R⌉ − 1.
        let t = ((offset + 1) * u64::from(total) - 1) / range;
        t as u32
    }

    /// Removes the decoded event `[cum_low, cum_high)` of `[0, total)` from the
    /// code string. The interval must hold `target(total)`, as the matching
    /// [`Encoder::encode`] call did. If it does not, the call fails with
    /// [`io::ErrorKind::InvalidInput`], and so does every later call.
    ///
    /// # Panics
    ///
    /// Panics unless `cum_low < cum_high <= total <= MAX_TOTAL`.
    pub fn consume(&mut self, cum_low: u32, cum_high: u32, total: u32) -> io::Result<()> {
        check_interval(cum_low, cum_high, total);
        if self.poisoned {
            return Err(poisoned_error());
        }
        narrow(&mut self.low, &mut self.high, cum_low, cum_high, total);
        if self.value < self.low || self.value > self.high {
            self.poisoned = true;
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "consume: interval does not contain the decoder's target value",
            ));
        }
        self.renormalize()
    }

    #[inline]
    fn renormalize(&mut self) -> io::Result<()> {
        let r = self.renormalize_inner();
        if r.is_err() {
            self.poisoned = true;
        }
        r
    }

    /// Mirrors the encoder's shifts, keeping `value` in the coordinates of
    /// `low` and `high`. Each shift reads one bit into `value`. The input
    /// already holds the settled bits, so nothing is pending.
    #[inline]
    fn renormalize_inner(&mut self) -> io::Result<()> {
        loop {
            if self.high < HALF {
                // nothing to subtract
            } else if self.low >= HALF {
                self.low -= HALF;
                self.high -= HALF;
                self.value -= HALF;
            } else if self.low >= QUARTER && self.high < THREE_QUARTERS {
                self.low -= QUARTER;
                self.high -= QUARTER;
                self.value -= QUARTER;
            } else {
                return Ok(());
            }
            self.low <<= 1;
            self.high = (self.high << 1) | 1;
            self.value = (self.value << 1) | u64::from(self.input.get()?);
        }
    }

    /// Decodes one symbol under `model`, then lets the model adapt.
    pub fn decode_symbol<M: Model + ?Sized>(&mut self, model: &mut M) -> io::Result<usize> {
        let total = model.total();
        let target = self.target(total);
        let (symbol, lo, hi) = model.find(target);
        self.consume(lo, hi, total)?;
        model.update(symbol);
        Ok(symbol)
    }

    /// Decodes a binary event with an adaptive [`BitModel`], then lets the
    /// model adapt.
    pub fn decode_bit<const S: u8>(&mut self, model: &mut BitModel<S>) -> io::Result<bool> {
        let bit = self.decode_split(model.split())?;
        model.update(bit);
        Ok(bit)
    }

    /// Decodes a binary event coded with [`Encoder::encode_bit_p`].
    ///
    /// # Panics
    ///
    /// Panics if `p1 == 0`.
    #[inline]
    pub fn decode_bit_p(&mut self, p1: u16) -> io::Result<bool> {
        assert!(p1 > 0, "p1 must be non-zero");
        self.decode_split((1u32 << BIT_PRECISION) - u32::from(p1))
    }

    /// Decodes a binary event of total 2¹⁶ without dividing; the inverse of
    /// `Encoder::encode_split`. With `mid = low + ⌊range·split / 2¹⁶⌋`:
    ///
    /// ```text
    /// target(2¹⁶) >= split  ⇔  (value − low + 1)·2¹⁶ > range·split
    ///                       ⇔  value − low >= ⌊range·split / 2¹⁶⌋
    ///                       ⇔  value >= mid
    /// ```
    #[inline]
    fn decode_split(&mut self, split: u32) -> io::Result<bool> {
        if self.poisoned {
            return Err(poisoned_error());
        }
        let range = self.high - self.low + 1;
        let mid = self.low + ((range * u64::from(split)) >> BIT_PRECISION);
        let bit = self.value >= mid;
        if bit {
            self.low = mid;
        } else {
            self.high = mid - 1;
        }
        self.renormalize()?;
        Ok(bit)
    }

    /// Reports whether the decoder sits where [`Encoder::finish`] leaves a
    /// well-formed code string: the code value equals the final `low`, which
    /// the encoder writes, and the unread pad bits of the current byte are
    /// zero.
    ///
    /// Checking this after the last event rejects other representations of the
    /// same events. It does not look for trailing bytes in the source.
    pub fn is_canonical_end(&self) -> bool {
        let mask = (1u16 << self.input.nbits) - 1;
        !self.poisoned && self.value == self.low && u16::from(self.input.byte) & mask == 0
    }

    /// Ends decoding and returns the source, positioned at the first byte after
    /// the code string.
    pub fn finish(self) -> R {
        self.input.inner
    }
}
