// Copyright (c) 2026 Darrell D. E. Long. All rights reserved.
// Use and redistribution are subject to the BSD 3-clause license, with a
// dedication clause, in the LICENSE file at the root of this repository.
//
// Dedicated to the memory of my colleague Glen G. Langdon

//! The arithmetic coding engine: interval narrowing, renormalisation,
//! carry (pending-bit) handling and termination.

use std::io::{self, Read, Write};

use crate::model::{BitModel, Model, BIT_PRECISION};

/// Width of the `low` / `high` registers, in bits.
const CODE_BITS: u32 = 32;
/// Largest value representable in a code register.
const TOP: u64 = (1 << CODE_BITS) - 1;
const HALF: u64 = 1 << (CODE_BITS - 1);
const QUARTER: u64 = 1 << (CODE_BITS - 2);
const THREE_QUARTERS: u64 = HALF + QUARTER;

/// Largest permissible `total` passed to [`Encoder::encode`] /
/// [`Decoder::target`].
///
/// After renormalisation the interval width always exceeds a quarter of the
/// code space (2³⁰), so any `total ≤ 2³⁰` guarantees every symbol with a
/// non-zero frequency receives a non-empty sub-interval, and every product
/// `width × cum` is at most 2⁶² and fits in a `u64`.
pub const MAX_TOTAL: u32 = 1 << (CODE_BITS - 2);

#[inline]
fn check_interval(cum_low: u32, cum_high: u32, total: u32) {
    assert!(
        cum_low < cum_high && cum_high <= total && total <= MAX_TOTAL,
        "invalid coding interval [{cum_low}, {cum_high}) of {total} (MAX_TOTAL = {MAX_TOTAL})"
    );
}

/// Narrow `[low, high]` to the sub-interval `[cum_low, cum_high)` of
/// `[0, total)`. Both endpoints use the old `low`, so `high` must be
/// assigned first. The width includes both endpoints; subtracting one at
/// the upper boundary converts the half-open model interval to that closed
/// representation. The `MAX_TOTAL` bound keeps every positive frequency
/// non-empty after integer rounding.
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

    /// Pad the final partial byte with zeros and return the sink.
    fn finish(mut self) -> io::Result<W> {
        if self.nbits > 0 {
            let b = self.byte << (8 - self.nbits);
            self.inner.write_all(&[b])?;
        }
        self.inner.flush()?;
        Ok(self.inner)
    }
}

/// Arithmetic encoder writing a bit stream into any [`Write`] sink.
///
/// The encoder writes one byte at a time; wrap unbuffered sinks (files,
/// sockets, stdout) in a [`std::io::BufWriter`].
///
/// Call [`Encoder::finish`] to write the tail of the code string; dropping
/// the encoder without finishing loses that tail. After an I/O failure, the internal state may no longer match the output;
/// later coding calls return an error, so recovery requires a new encoder.
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
    /// Create an encoder writing to `sink`.
    pub fn new(sink: W) -> Self {
        Encoder { low: 0, high: TOP, pending: 0, out: BitWriter::new(sink), poisoned: false }
    }

    /// Encode one event occupying the sub-interval `[cum_low, cum_high)` of
    /// `[0, total)`.
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

    /// Encode `symbol` under `model`, then let the model adapt.
    pub fn encode_symbol<M: Model + ?Sized>(&mut self, model: &mut M, symbol: usize) -> io::Result<()> {
        let (lo, hi) = model.interval(symbol);
        self.encode(lo, hi, model.total())?;
        model.update(symbol);
        Ok(())
    }

    /// Encode a single binary event with an adaptive [`BitModel`] (Langdon's
    /// binary arithmetic code), then let the model adapt.
    pub fn encode_bit<const S: u8>(&mut self, model: &mut BitModel<S>, bit: bool) -> io::Result<()> {
        self.encode_split(model.split(), bit)?;
        model.update(bit);
        Ok(())
    }

    /// Encode a binary event whose probability of being 1 is
    /// `p1 / 2^16`, as supplied by an external predictor (e.g. a
    /// context-mixing model). The decoder must call
    /// [`Decoder::decode_bit_p`] with the same `p1`.
    ///
    /// # Panics
    ///
    /// Panics if `p1 == 0` (a 1 would be uncodable).
    #[inline]
    pub fn encode_bit_p(&mut self, bit: bool, p1: u16) -> io::Result<()> {
        assert!(p1 > 0, "p1 must be non-zero");
        self.encode_split((1u32 << BIT_PRECISION) - u32::from(p1), bit)
    }

    /// Binary event with total 2^16: identical to
    /// `encode(0, split, 2^16)` / `encode(split, 2^16, 2^16)` but the
    /// division by a power of two is a shift.
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

    /// Resolve the leading bit and all earlier middle-half shifts. Each
    /// deferred bit is its complement: a resolved 0 with three pending
    /// shifts emits 0111, while a resolved 1 emits 1000.
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

    /// Restore precision after narrowing (Witten–Neal–Cleary E1/E2/E3).
    /// An interval in one half has a settled leading bit, which can be
    /// emitted and shifted out. An interval confined to the middle half
    /// has no settled bit yet: remove its second bit and defer the carry
    /// decision in `pending`. Each case doubles the remaining interval.
    /// On exit it straddles HALF and extends outside the middle half, so
    /// its width exceeds QUARTER, as required by the next event's rounding.
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
                // Interval straddles the midpoint but is confined to the
                // middle half: the next bit depends on a future carry.
                self.pending += 1;
                self.low -= QUARTER;
                self.high -= QUARTER;
            } else {
                return Ok(());
            }
            self.low <<= 1;
            // Each old integer point expands to two points; include both
            // at the upper endpoint of the closed interval.
            self.high = (self.high << 1) | 1;
        }
    }

    /// Terminate the code string, flush, and return the underlying sink.
    ///
    /// The encoder emits the full 32-bit value of `low` (plus any pending
    /// bits). `low` lies in the final interval, and because the decoder
    /// reads exactly 32 bits up front plus one per renormalisation shift, it
    /// will consume exactly the bits written here — never more. The final
    /// partial byte is zero-padded; the decoder discards those pad bits.
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

    /// Borrow the underlying sink.
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

/// Arithmetic decoder reading a bit stream from any [`Read`] source.
///
/// The decoder reads one byte at a time. Provided it is driven through
/// exactly the same sequence of events as the encoder (so it knows when the
/// message ends — by a length, an end-of-stream symbol, or framing), it
/// stops exactly at the end of the code string produced by
/// [`Encoder::finish`] and never reads beyond it, so the source may continue
/// with other data (see [`Decoder::finish`]). A code string is *not*
/// self-delimiting on its own. Wrap unbuffered sources in a
/// [`std::io::BufReader`].
///
/// After an I/O failure or an interval that does not contain the code
/// value, later coding calls return an error. Retrying would use a state
/// that may no longer match the input position.
///
/// Decoding is a two-step protocol, which lets any model drive it:
/// call [`Decoder::target`] to obtain a value in `[0, total)`, map it to the
/// symbol whose interval `[cum_low, cum_high)` contains it, then call
/// [`Decoder::consume`] with that interval. [`Decoder::decode_symbol`] does
/// both for a [`Model`].
pub struct Decoder<R: Read> {
    low: u64,
    high: u64,
    /// Invariant while not poisoned: `low <= value <= high`.
    value: u64,
    input: BitReader<R>,
    poisoned: bool,
}

impl<R: Read> Decoder<R> {
    /// Create a decoder reading from `source`. Reads the first 32 bits.
    pub fn new(source: R) -> io::Result<Self> {
        let mut input = BitReader::new(source);
        let mut value = 0u64;
        for _ in 0..CODE_BITS {
            value = (value << 1) | u64::from(input.get()?);
        }
        Ok(Decoder { low: 0, high: TOP, value, input, poisoned: false })
    }

    /// Return the scaled position of the code value within the current
    /// interval, in `[0, total)`. The caller finds the symbol whose
    /// cumulative interval contains this value.
    ///
    /// While the decoder is healthy the invariant `low <= value <= high`
    /// guarantees the result is `< total`, even for corrupt input. After an
    /// error the result is meaningless (but still `< total`, and never
    /// panics); the following [`Decoder::consume`] reports the error.
    ///
    /// # Panics
    ///
    /// Panics if `total == 0` or `total > MAX_TOTAL`.
    pub fn target(&self, total: u32) -> u32 {
        assert!(total > 0 && total <= MAX_TOTAL, "invalid total {total}");
        // The clamps keep this read-only query defined after an error;
        // they do not alter a healthy decoder's interval or offset.
        let range = self.high.wrapping_sub(self.low).wrapping_add(1).max(1);
        let offset = self.value.saturating_sub(self.low).min(range - 1);
        // Invert narrow's rounded boundaries: ceil((offset + 1) * T / R)
        // minus one selects the correct event even on an integer boundary.
        let t = ((offset + 1) * u64::from(total) - 1) / range;
        t as u32
    }

    /// Remove the decoded event `[cum_low, cum_high)` of `[0, total)` from
    /// the code string. Must match the [`Encoder::encode`] call exactly.
    ///
    /// The interval **must contain** `self.target(total)`, i.e.
    /// `cum_low <= target(total) < cum_high`. Any other interval would
    /// move the code value outside the coder's interval. This is detected:
    /// the call returns an [`io::ErrorKind::InvalidInput`] error and the
    /// decoder is poisoned.
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

    /// Mirror the encoder's half/middle-half shifts while keeping `value`
    /// in the same coordinates as `low` and `high`. Each shift reads one
    /// new bit into the low end of `value`. No pending-bit counter is
    /// needed here: the input already contains the encoder's resolved bits.
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

    /// Decode one symbol under `model`, then let the model adapt.
    pub fn decode_symbol<M: Model + ?Sized>(&mut self, model: &mut M) -> io::Result<usize> {
        let total = model.total();
        let target = self.target(total);
        let (symbol, lo, hi) = model.find(target);
        self.consume(lo, hi, total)?;
        model.update(symbol);
        Ok(symbol)
    }

    /// Decode a single binary event with an adaptive [`BitModel`], then let
    /// the model adapt.
    pub fn decode_bit<const S: u8>(&mut self, model: &mut BitModel<S>) -> io::Result<bool> {
        let bit = self.decode_split(model.split())?;
        model.update(bit);
        Ok(bit)
    }

    /// Decode a binary event encoded with [`Encoder::encode_bit_p`].
    ///
    /// # Panics
    ///
    /// Panics if `p1 == 0`.
    #[inline]
    pub fn decode_bit_p(&mut self, p1: u16) -> io::Result<bool> {
        assert!(p1 > 0, "p1 must be non-zero");
        self.decode_split((1u32 << BIT_PRECISION) - u32::from(p1))
    }

    /// Division-free binary decode, exactly inverse to
    /// `Encoder::encode_split`. With `mid = low + ⌊range·split / 2¹⁶⌋`,
    /// `target(2¹⁶) >= split  ⇔  (value−low+1)·2¹⁶ > range·split
    ///  ⇔  value−low >= ⌊range·split / 2¹⁶⌋  ⇔  value >= mid`.
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

    /// `true` if the decoder sits exactly where [`Encoder::finish`] leaves
    /// a well-formed code string: the code value equals the final `low`
    /// (the encoder writes `low` itself) and the unread pad bits of the
    /// current byte are zero.
    ///
    /// Check after the last event to reject alternative representations
    /// of the same event sequence. This does not check for trailing bytes
    /// in the source; the caller must check its payload boundary separately.
    pub fn is_canonical_end(&self) -> bool {
        let mask = (1u16 << self.input.nbits) - 1;
        !self.poisoned && self.value == self.low && u16::from(self.input.byte) & mask == 0
    }

    /// Finish decoding and return the underlying source.
    ///
    /// The source is positioned at the first byte after the code string
    /// (the zero pad bits of the last byte are discarded).
    pub fn finish(self) -> R {
        self.input.inner
    }
}
