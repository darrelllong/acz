// Copyright (c) 2026 Darrell D. E. Long. All rights reserved.
// Use and redistribution are subject to the BSD 3-clause license, with a
// dedication clause, in the LICENSE file at the root of this repository.
//
// Dedicated to the memory of my colleague Glen G. Langdon

//! Probability models that drive the coder.

use crate::coder::MAX_TOTAL;

/// A model over the symbols `0..num_symbols()`, adaptive or static.
///
/// The model is the statistics unit of Langdon's compression system: it tells
/// the coder which sub-interval of `[0, total)` each symbol occupies. The
/// encoder and the decoder must drive identical models through identical calls.
///
/// An implementation must keep `0 < total() <= MAX_TOTAL`. The intervals of its
/// symbols must partition `[0, total())`, every symbol that can occur must have
/// a non-empty interval, and `find(t)` must return the symbol whose interval
/// holds `t`.
pub trait Model {
    /// Returns the number of symbols.
    fn num_symbols(&self) -> usize;
    /// Returns the sum of the frequencies.
    fn total(&self) -> u32;
    /// Returns the interval `[cum_low, cum_high)` of `symbol`.
    fn interval(&self, symbol: usize) -> (u32, u32);
    /// Returns the symbol whose interval holds `target`, and that interval.
    fn find(&self, target: u32) -> (usize, u32, u32);
    /// Adapts after `symbol` is coded. A static model does nothing.
    fn update(&mut self, symbol: usize);
}

/// A table of frequency counts, adaptive or static, kept in a Fenwick tree (P.
/// M. Fenwick, *A New Data Structure for Cumulative Frequency Tables*,
/// Software—Practice and Experience 24(3), 1994, pp. 327–336). Finding a
/// symbol's interval, finding the symbol for a target, and changing a count
/// take *O(log n)*. Halving the counts takes *O(n)*.
///
/// [`Self::new`] starts every symbol at frequency one; the other constructors
/// take counts. An adaptive table adds `increment` to a symbol's count each
/// time it is coded. When the total passes `limit`, every count is halved,
/// rounding up so that a positive count stays positive. That bounds the
/// precision needed and favors recent observations.
#[derive(Clone, Debug)]
pub struct FrequencyTable {
    freq: Vec<u32>,
    /// One-indexed partial sums: `tree[i]` covers the `i & -i` frequencies
    /// ending at `freq[i - 1]`. Index zero is unused.
    tree: Vec<u32>,
    total: u32,
    increment: u32,
    limit: u32,
    /// The largest power of two not above `freq.len()`, where the search for a
    /// target starts.
    top: usize,
}

impl FrequencyTable {
    /// The count added per coded symbol by default.
    pub const DEFAULT_INCREMENT: u32 = 32;
    /// The total at which counts are halved by default.
    pub const DEFAULT_LIMIT: u32 = 1 << 16;

    /// Returns an adaptive table over `n` symbols, all equally likely at first.
    ///
    /// # Panics
    ///
    /// Panics if `n == 0` or `2 * n + DEFAULT_INCREMENT > DEFAULT_LIMIT` (more
    /// than 32,752 symbols).
    pub fn new(n: usize) -> Self {
        Self::with_params(&vec![1; n], Self::DEFAULT_INCREMENT, Self::DEFAULT_LIMIT)
    }

    /// Returns a static table with the given counts. A symbol with a zero count
    /// cannot be coded. Counts whose sum exceeds [`MAX_TOTAL`] are scaled down
    /// in proportion.
    ///
    /// # Panics
    ///
    /// Panics if `counts` is empty or all zero.
    pub fn from_counts(counts: &[u32]) -> Self {
        let sum: u64 = counts.iter().map(|&c| u64::from(c)).sum();
        assert!(sum > 0, "at least one count must be non-zero");
        let scaled: Vec<u32> = if sum > u64::from(MAX_TOTAL) {
            // Scale so the sum fits, keeping non-zero counts non-zero.
            let n = counts.len() as u64;
            let budget = u64::from(MAX_TOTAL) - n;
            counts.iter().map(|&c| if c == 0 { 0 } else { (u64::from(c) * budget / sum).max(1) as u32 }).collect()
        } else {
            counts.to_vec()
        };
        Self::with_params(&scaled, 0, MAX_TOTAL)
    }

    /// Returns a table with the given initial counts, the increment added per
    /// coded symbol (0 for a static table), and the total at which counts are
    /// halved.
    ///
    /// # Panics
    ///
    /// Panics if `counts` is empty or sums to zero, if `limit` is not in
    /// `sum(counts) ..= MAX_TOTAL`, or if halving could not bring the total
    /// back under `limit` (`limit < 2 * counts.len() + increment` for an
    /// adaptive table).
    pub fn with_params(counts: &[u32], increment: u32, limit: u32) -> Self {
        let n = counts.len();
        assert!(n > 0, "alphabet must be non-empty");
        assert!(limit <= MAX_TOTAL, "limit exceeds MAX_TOTAL");
        let total: u64 = counts.iter().map(|&c| u64::from(c)).sum();
        assert!(total > 0, "at least one count must be non-zero");
        assert!(total <= u64::from(limit), "initial counts exceed limit");
        if increment > 0 {
            assert!(
                u64::from(limit) >= 2 * n as u64 + u64::from(increment),
                "limit too small for alphabet size and increment"
            );
        }
        let mut t = FrequencyTable {
            freq: counts.to_vec(),
            tree: vec![0; n + 1],
            total: total as u32,
            increment,
            limit,
            top: 1 << (usize::BITS - 1 - n.leading_zeros()),
        };
        t.rebuild();
        t
    }

    /// Rebuilds the tree in linear time. Each node starts with its own
    /// frequency and passes its finished sum to the next enclosing block. A
    /// parent has a larger index than its children, so each child is finished
    /// before its parent is visited.
    fn rebuild(&mut self) {
        let n = self.freq.len();
        self.tree[1..].copy_from_slice(&self.freq);
        self.tree[0] = 0;
        for i in 1..=n {
            let parent = i + (i & i.wrapping_neg());
            if parent <= n {
                self.tree[parent] += self.tree[i];
            }
        }
        self.total = self.freq.iter().sum();
    }

    /// Returns the sum of the frequencies of symbols `0..symbol`. `tree[i]`
    /// covers the block ending at `freq[i - 1]`, and clearing the lowest set
    /// bit of `i` moves to the end of the block before it, so the blocks
    /// visited partition the prefix.
    fn prefix(&self, symbol: usize) -> u32 {
        let mut i = symbol;
        let mut s = 0;
        while i > 0 {
            s += self.tree[i];
            i &= i - 1;
        }
        s
    }

    /// Returns the frequency of `symbol`.
    pub fn frequency(&self, symbol: usize) -> u32 {
        self.freq[symbol]
    }

    /// Adds `delta` to the count of `symbol`, and halves every count if the
    /// total passes the limit. Adding the lowest set bit of the index visits
    /// each enclosing block. Halving changes every frequency, so the tree is
    /// rebuilt.
    pub fn increment(&mut self, symbol: usize, delta: u32) {
        let n = self.freq.len();
        self.freq[symbol] += delta;
        self.total += delta;
        let mut i = symbol + 1;
        while i <= n {
            self.tree[i] += delta;
            i += i & i.wrapping_neg();
        }
        if self.total > self.limit {
            for f in &mut self.freq {
                *f = f.div_ceil(2);
            }
            self.rebuild();
        }
    }
}

impl Model for FrequencyTable {
    fn num_symbols(&self) -> usize {
        self.freq.len()
    }

    fn total(&self) -> u32 {
        self.total
    }

    fn interval(&self, symbol: usize) -> (u32, u32) {
        let lo = self.prefix(symbol);
        let hi = lo + self.freq[symbol];
        (lo, hi)
    }

    fn find(&self, target: u32) -> (usize, u32, u32) {
        // Find the longest prefix whose sum does not exceed target, keeping
        // rem = target − prefix(pos). The steps are descending powers of
        // two, so tree[next] is the block that would extend the prefix to
        // next.
        let n = self.freq.len();
        let mut pos = 0usize;
        let mut rem = target;
        let mut step = self.top;
        while step > 0 {
            let next = pos + step;
            if next <= n && self.tree[next] <= rem {
                pos = next;
                rem -= self.tree[next];
            }
            step >>= 1;
        }
        // Symbol pos would pass target. Equality was accepted above, which
        // skips symbols of zero frequency, so pos is the symbol whose
        // interval holds target.
        let lo = target - rem;
        (pos, lo, lo + self.freq[pos])
    }

    fn update(&mut self, symbol: usize) {
        if self.increment > 0 {
            self.increment(symbol, self.increment);
        }
    }
}

/// Precision, in bits, of [`BitModel`] probabilities.
pub const BIT_PRECISION: u32 = 16;

/// An adaptive probability for a binary event: the skew of Langdon's binary
/// arithmetic code, generalized from a power of two to a 16-bit probability.
///
/// After each event the estimate moves `2^-SHIFT` of the way toward the bit
/// seen. A small `SHIFT` adapts fast; a large one averages over more events.
/// The probability stays strictly inside `(0, 1)`, so both outcomes remain
/// codable. The model is two bytes.
///
/// `SHIFT` must be in `1..=15`. Anything else fails to compile:
///
/// ```compile_fail
/// let _ = arith::BitModel::<0>::new();
/// ```
#[derive(Clone, Copy, Debug)]
pub struct BitModel<const SHIFT: u8 = 4> {
    /// P(bit = 1) × 2^16.
    p1: u16,
}

impl<const SHIFT: u8> Default for BitModel<SHIFT> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const SHIFT: u8> BitModel<SHIFT> {
    /// P(1) = ½. Evaluating it rejects an invalid `SHIFT` at compile time.
    const HALF: u16 = {
        assert!(SHIFT >= 1 && SHIFT <= 15, "SHIFT must be in 1..=15");
        1 << (BIT_PRECISION - 1)
    };

    /// Returns a model with P(1) = ½.
    pub const fn new() -> Self {
        BitModel { p1: Self::HALF }
    }

    /// Returns the probability of a 1, as a fraction of 2¹⁶.
    pub fn p1(&self) -> u16 {
        self.p1
    }

    /// Returns the boundary between the two parts of `[0, 2^16)`: a 0 takes
    /// `[0, split)` and a 1 takes `[split, 2^16)`.
    #[inline]
    pub(crate) fn split(&self) -> u32 {
        (1u32 << BIT_PRECISION) - u32::from(self.p1)
    }

    /// Adapts toward `bit`.
    #[inline]
    pub fn update(&mut self, bit: bool) {
        // p1 stays within [2^SHIFT - 1, 2^16 - 2^SHIFT + 1] ⊂ (0, 2^16).
        if bit {
            self.p1 += (((1u32 << BIT_PRECISION) - u32::from(self.p1)) >> SHIFT) as u16;
        } else {
            self.p1 -= self.p1 >> SHIFT;
        }
    }
}
