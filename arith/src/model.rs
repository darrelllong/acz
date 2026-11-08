// Copyright (c) 2026 Darrell D. E. Long. All rights reserved.
// Use and redistribution are subject to the BSD 3-clause license, with a
// dedication clause, in the LICENSE file at the root of this repository.
//
// Dedicated to the memory of my colleague Glen G. Langdon

//! Probability models that drive the coder.

use crate::coder::MAX_TOTAL;

/// A (possibly adaptive) model over the symbols `0..num_symbols()`.
///
/// The model is the "statistics unit" in Langdon's compression system: it
/// tells the coder what sub-interval of `[0, total)` each symbol occupies.
/// The encoder and decoder must drive identical models through identical
/// sequences of calls.
///
/// Invariants an implementation must uphold:
/// * `0 < total() <= MAX_TOTAL`;
/// * the intervals of all symbols partition `[0, total())` (no gaps, no
///   overlaps), and every symbol that can occur has a non-empty interval;
/// * `find(t)` returns the symbol whose interval contains `t`.
pub trait Model {
    /// Number of symbols in the alphabet.
    fn num_symbols(&self) -> usize;
    /// Sum of all frequencies.
    fn total(&self) -> u32;
    /// Cumulative interval `[cum_low, cum_high)` of `symbol`.
    fn interval(&self, symbol: usize) -> (u32, u32);
    /// Symbol whose interval contains `target`, with that interval.
    fn find(&self, target: u32) -> (usize, u32, u32);
    /// Adapt after coding `symbol`. Static models do nothing.
    fn update(&mut self, symbol: usize);
}

/// An adaptive (or static) frequency-count model backed by a Fenwick tree
/// (P. M. Fenwick, *A New Data Structure for Cumulative Frequency Tables*,
/// Software—Practice and Experience 24(3), 1994, pp. 327–336).
/// Interval lookup, inverse lookup and individual count changes take
/// *O(log n)* time; rescaling all counts takes *O(n)*.
///
/// [`Self::new`] starts every symbol at frequency one; the other constructors
/// accept supplied counts. An adaptive table adds `increment` after each
/// symbol. When the total exceeds `limit`, all counts are halved, rounding
/// up so positive counts stay positive. This bounds precision and gives
/// recent observations more influence than old ones.
#[derive(Clone, Debug)]
pub struct FrequencyTable {
    freq: Vec<u32>,
    /// One-indexed partial sums: `tree[i]` covers the `i & -i` frequencies
    /// ending at `freq[i - 1]`. Index zero is unused.
    tree: Vec<u32>,
    total: u32,
    increment: u32,
    limit: u32,
    /// Largest power of two `<= freq.len()`, for binary-lifting search.
    top: usize,
}

impl FrequencyTable {
    /// Default count increment per coded symbol.
    pub const DEFAULT_INCREMENT: u32 = 32;
    /// Default rescaling threshold for the total.
    pub const DEFAULT_LIMIT: u32 = 1 << 16;

    /// Adaptive model over `n` symbols, all initially equiprobable.
    ///
    /// # Panics
    ///
    /// Panics if `n == 0` or `2 * n + DEFAULT_INCREMENT > DEFAULT_LIMIT`
    /// (more than 32,752 symbols).
    pub fn new(n: usize) -> Self {
        Self::with_params(&vec![1; n], Self::DEFAULT_INCREMENT, Self::DEFAULT_LIMIT)
    }

    /// Static model with the given counts (never adapts). Zero counts are
    /// allowed — those symbols simply cannot be encoded. Counts are scaled
    /// down proportionally if their sum exceeds [`MAX_TOTAL`].
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

    /// Fully general constructor: initial counts, per-symbol increment, and
    /// the total at which counts are halved.
    ///
    /// # Panics
    ///
    /// Panics if `counts` is empty, sums to zero, or `limit` is not in
    /// `sum(counts) ..= MAX_TOTAL`, or if halving could not bring the
    /// total back under `limit` (`limit < 2 * counts.len() + increment`
    /// for adaptive tables).
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

    /// Rebuild in linear time after construction or rescaling. Start each
    /// node with its own frequency, then pass its completed sum to the
    /// next enclosing block. Parents have larger indices, so each child
    /// is complete before its parent is visited.
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

    /// Sum of frequencies of symbols `0..symbol`. `tree[i]` covers the
    /// block ending at `freq[i - 1]`. Clearing the lowest set bit moves
    /// to the end of the preceding block, so the visited blocks partition
    /// the prefix without overlap.
    fn prefix(&self, symbol: usize) -> u32 {
        let mut i = symbol;
        let mut s = 0;
        while i > 0 {
            s += self.tree[i];
            i &= i - 1;
        }
        s
    }

    /// Current frequency of `symbol`.
    pub fn frequency(&self, symbol: usize) -> u32 {
        self.freq[symbol]
    }

    /// Add `delta` to `symbol`'s count, rescaling if the limit is exceeded.
    /// Adding the lowest set bit visits each enclosing block that contains
    /// this symbol. A rescale changes every frequency, so it rebuilds the
    /// tree in one linear pass rather than issuing separate updates.
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
        // Find the largest prefix whose sum does not exceed target.
        // Throughout, rem = target - prefix(pos). Descending powers of
        // two keep next aligned so tree[next] is exactly the additional
        // block to try. Accepting it skips that block and subtracts its
        // sum, giving one O(log n) descent instead of repeated prefix sums.
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
        // Adding symbol pos would now exceed target. Accepting equality
        // in the search skips zero-frequency symbols as well, so pos is
        // the symbol whose nonempty interval contains target.
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

/// Adaptive probability for a binary event — the "skew" parameter of
/// Langdon's binary arithmetic code, generalised from powers of two to a
/// 16-bit fixed-point probability.
///
/// After each event the estimate moves a fraction `2^-SHIFT` of the way
/// toward the observed bit (an exponentially-weighted moving average).
/// Small `SHIFT` adapts fast; large `SHIFT` averages over a longer window.
/// The probability always stays strictly inside `(0, 1)`, so both outcomes
/// remain codable. The struct is two bytes, so large context tables of
/// binary models are cheap.
///
/// `SHIFT` must be in `1..=15`; anything else fails to compile:
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
    /// P(1) = ½; evaluating it also rejects an invalid `SHIFT` at compile time.
    const HALF: u16 = {
        assert!(SHIFT >= 1 && SHIFT <= 15, "SHIFT must be in 1..=15");
        1 << (BIT_PRECISION - 1)
    };

    /// A model with P(1) = ½.
    pub const fn new() -> Self {
        BitModel { p1: Self::HALF }
    }

    /// Probability of a 1 bit, as a fraction of 2^16.
    pub fn p1(&self) -> u16 {
        self.p1
    }

    /// Boundary between the 0 and 1 sub-intervals of `[0, 2^16)`:
    /// a 0 occupies `[0, split)`, a 1 occupies `[split, 2^16)`.
    #[inline]
    pub(crate) fn split(&self) -> u32 {
        (1u32 << BIT_PRECISION) - u32::from(self.p1)
    }

    /// Adapt toward the observed `bit`.
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
