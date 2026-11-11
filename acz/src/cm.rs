// Copyright (c) 2026 Darrell D. E. Long. All rights reserved.
// Use and redistribution are subject to the BSD 3-clause license, with a
// dedication clause, in the LICENSE file at the root of this repository.
//
// Dedicated to the memory of my colleague Glen G. Langdon

//! A context-mixing bit predictor, in integer arithmetic so that encoder and
//! decoder agree on every platform. The architecture is that of the PAQ
//! compressors as their published descriptions give it (Mahoney 2005; Knoll and
//! de Freitas 2012).
//!
//! Each byte is coded as eight binary decisions, most significant bit first.
//! For each bit:
//!
//! 1. The context models give probabilities. Orders 0 and 1 are counters
//!    indexed directly. Seven hashed models (orders 2, 3, 4 and 6, the current
//!    word, and two sparse contexts for binary data) each keep, for a context,
//!    an adaptive probability and a bit history that a learned map turns into a
//!    second probability. A match model predicts the bit that followed the most
//!    recent occurrence of the last six bytes.
//!
//! 2. Two logistic mixers combine these in the stretched domain,
//!    `st(p) = ln(p / (1 − p))`, as `p = squash(Σ wᵢ · st(pᵢ))`. Their weights
//!    are trained by gradient descent on coding cost. One chooses its weights
//!    by match length and partial byte, the other by the previous byte, and
//!    their outputs are averaged in the stretched domain.
//!
//! 3. Two adaptive probability maps, in order-0 and order-1 context, correct
//!    the result. The final probability is `(p + apm₀ + 2·apm₁) / 4`.
//!
//! # References
//!
//! M. V. Mahoney, *Adaptive Weighing of Context Models for Lossless Data
//! Compression*, Florida Institute of Technology Technical Report CS-2005-16,
//! 2005 (bit histories; secondary estimation).
//!
//! B. Knoll and N. de Freitas, *A Machine Learning Perspective on Predictive
//! Coding with PAQ8*, Proc. Data Compression Conference, 2012 (logistic mixing
//! with weights chosen by context; adaptive probability maps).

/// The scale of a 12-bit probability.
const PSCALE: i32 = 4096;

/// A stretched probability lies within ±`STRETCH_MAX`.
const STRETCH_MAX: i32 = 2047;

/// [`squash`] of every `d` in `−2047..=2047`, at index `d + 2047`.
static SQUASH: [u16; 4095] = squash_table();

/// [`stretch`] of every 12-bit probability.
static STRETCH: [i16; 4096] = stretch_table();

/// Tabulates the logistic function `4096 / (1 + e^(−d/256))`, rounded to the
/// nearest integer and kept within `1..=4095`.
///
/// The arithmetic is in integers, so the table is the same on every platform.
/// `e^(1/256)` is summed from its power series in fixed point, and `e^(d/256)`
/// follows by repeated multiplication. For a negative `d` the symmetry of the
/// function gives `4096 − squash(−d)`.
const fn squash_table() -> [u16; 4095] {
    const FRAC: u32 = 56;
    const ONE: u128 = 1 << FRAC;
    let mut g = 0;
    let mut term = ONE;
    let mut k = 1;
    while term > 0 {
        g += term;
        term /= 256 * k;
        k += 1;
    }
    let mut t = [0u16; 4095];
    // e^(d/256), below 2^12 for every d here.
    let mut e = ONE;
    let mut d = 0;
    while d <= 2047 {
        let p = (4096 * e + (e + ONE) / 2) / (e + ONE);
        let p = if p > 4095 { 4095 } else { p as u16 };
        t[2047 + d] = p;
        t[2047 - d] = 4096 - p;
        e = (e * g) >> FRAC;
        d += 1;
    }
    t
}

/// Tabulates the inverse of [`squash`]: for each `p`, the `d` whose squash is
/// nearest `p`, and of several the one nearest zero. For `p` below a half the
/// symmetry gives `−stretch(4096 − p)`.
const fn stretch_table() -> [i16; 4096] {
    let sq = squash_table();
    let mut t = [0i16; 4096];
    // The least d >= 0 whose squash is at least p.
    let mut above = 0;
    let mut p = 2048;
    while p < 4096 {
        while (sq[2047 + above] as usize) < p {
            above += 1;
        }
        let mut best = above;
        if (sq[2047 + above] as usize) > p {
            // The value below p, at the least d that has it.
            let mut below = above - 1;
            while below > 0 && sq[2047 + below - 1] == sq[2047 + below] {
                below -= 1;
            }
            if p - (sq[2047 + below] as usize) <= (sq[2047 + above] as usize) - p {
                best = below;
            }
        }
        t[p] = best as i16;
        t[4096 - p] = -(best as i16);
        p += 1;
    }
    t[0] = -(STRETCH_MAX as i16);
    t
}

/// Returns `4096 / (1 + e^(−d/256))` as a 12-bit probability in `1..=4095`.
#[inline]
fn squash(d: i32) -> i32 {
    i32::from(SQUASH[(d.clamp(-STRETCH_MAX, STRETCH_MAX) + STRETCH_MAX) as usize])
}

/// Returns the inverse of [`squash`], `256 · ln(p / (4096 − p))`, for a 12-bit
/// probability. The mask changes nothing for such a value and spares the
/// lookup a bounds check.
#[inline]
fn stretch(p12: i32) -> i32 {
    debug_assert!((0..PSCALE).contains(&p12));
    i32::from(STRETCH[p12 as usize & (PSCALE as usize - 1)])
}

/// The learning rate after `n` observations, `65536 / (n + 3.5)`. A counter
/// that moves `1/(n + 3.5)` of the way toward each bit stays close to the
/// frequency of what it has seen: it learns fast when new and settles as
/// evidence accumulates. The 3.5 keeps one observation from taking it to 0 or
/// 1.
static RECIP: [u32; 1024] = recip_table();

const fn recip_table() -> [u32; 1024] {
    let mut t = [0u32; 1024];
    let mut n = 0;
    while n < 1024 {
        let v = 131_072 / (2 * n as u32 + 7);
        t[n] = if v > 1 { v } else { 1 };
        n += 1;
    }
    t
}

/// A counter is a `u32`: a 16-bit probability of a 1 in the high half and
/// its observation count in the low half. It starts at ½, never observed.
const COUNTER_INIT: u32 = 0x8000_0000;

/// Returns a counter's probability in 12 bits.
#[inline]
fn counter_p(slot: u32) -> i32 {
    (slot >> 20) as i32
}

/// Moves a counter `1/(n + 3.5)` of the way toward `bit` (see [`RECIP`]). The
/// count stops at `limit`, so the step never shrinks to nothing and the counter
/// can follow data that changes. A small limit tracks recent statistics; a
/// large one averages longer.
#[inline]
fn counter_update(slot: &mut u32, bit: bool, limit: u32) {
    let p = i64::from(*slot >> 16);
    let n = *slot & 0xFFFF;
    let target = if bit { 65535 } else { 0 };
    // No limit reaches 1024, so the mask changes nothing. It spares the
    // lookup a bounds check.
    let np = p + (((target - p) * i64::from(RECIP[n as usize & 1023])) >> 16);
    let nn = if n < limit { n + 1 } else { n };
    *slot = ((np as u32) << 16) | nn;
}

/// The most a bit-history count can reach.
const HIST_CAP: u32 = 63;
const _: () = assert!(2 * HIST_CAP < 1024, "n0 + n1 must index the reciprocal table");

// A slot of a hashed model holds a 16-bit adaptive probability in its high
// half and a bit history in its low half: the counts `n0` (bits 8..16) and
// `n1` (bits 0..8) of zeros and ones seen (Mahoney 2005, §3). A probability
// alone does not say how much evidence stands behind it. The history does,
// and a learned map (`HistMap`) turns it into a second prediction.

/// Returns the index of a slot's bit history in a map of [`HIST_STATES`].
#[inline]
fn hist_index(slot: u32) -> usize {
    let n0 = (slot >> 8) & 0xFF;
    let n1 = slot & 0xFF;
    (n0 * (HIST_CAP + 1) + n1) as usize
}

/// Learns `bit` in a slot. The probability moves as a counter's does, with
/// `n0 + n1` as the count. The history is semi-stationary (Mahoney 2005): a 1
/// adds to `n1` and, if `n0 > 2`, cuts `n0` to `n0 / 2 + 1`, and a 0 does the
/// mirror image. Evidence against the latest outcome is discounted but kept, so
/// a context that changes is believed again within a few bits.
#[inline]
fn hist_update(slot: &mut u32, bit: bool) {
    let p = i64::from(*slot >> 16);
    let mut n0 = (*slot >> 8) & 0xFF;
    let mut n1 = *slot & 0xFF;
    let n = (n0 + n1) as usize;
    let target = if bit { 65535 } else { 0 };
    let np = p + (((target - p) * i64::from(RECIP[n])) >> 16);
    if bit {
        n1 = (n1 + 1).min(HIST_CAP);
        if n0 > 2 {
            n0 = n0 / 2 + 1;
        }
    } else {
        n0 = (n0 + 1).min(HIST_CAP);
        if n1 > 2 {
            n1 = n1 / 2 + 1;
        }
    }
    *slot = ((np as u32) << 16) | (n0 << 8) | n1;
}

/// What each bit history predicts, learned as coding goes. Every model has its
/// own map, because three ones and no zeros mean one thing in an order-2
/// context and another in an order-6 context.
struct HistMap {
    /// One map of [`HIST_STATES`] counters per model, one after another.
    t: Vec<u32>,
}

/// The number of bit histories `(n0, n1)`.
const HIST_STATES: usize = ((HIST_CAP + 1) * (HIST_CAP + 1)) as usize;

impl HistMap {
    fn new(models: usize) -> Self {
        let k = HIST_CAP + 1;
        let mut one = vec![0u32; HIST_STATES];
        for n0 in 0..k {
            for n1 in 0..k {
                // Prior (n1 + ½) / (n0 + n1 + 1), as a 16-bit probability.
                let p = ((2 * n1 + 1) << 16) / (2 * (n0 + n1) + 2);
                one[(n0 * k + n1) as usize] = p.min(65535) << 16;
            }
        }
        HistMap { t: one.repeat(models) }
    }

    #[inline]
    fn p(&self, i: usize) -> i32 {
        (self.t[i] >> 20) as i32
    }

    #[inline]
    fn update(&mut self, i: usize, bit: bool) {
        counter_update(&mut self.t[i], bit, 1023);
    }
}

/// A table of counters indexed directly.
struct Counters {
    t: Vec<u32>,
    limit: u32,
}

impl Counters {
    fn new(size: usize, limit: u32) -> Self {
        Counters { t: vec![COUNTER_INIT; size], limit }
    }

    #[inline]
    fn p(&self, i: usize) -> i32 {
        counter_p(self.t[i])
    }

    #[inline]
    fn update(&mut self, i: usize, bit: bool) {
        counter_update(&mut self.t[i], bit, self.limit);
    }
}

/// One cache line of slots for one hashed context and one nibble. Slot 0 is
/// metadata. Slots 1 to 15 are the nodes of the binary tree over the nibble's
/// four bits; a node is the bits seen so far behind a leading 1. A context is
/// looked up once for each nibble, and the slots for its four bits share the
/// line.
#[repr(align(64))]
#[derive(Clone, Copy)]
struct Bucket([u32; 16]);

/// The hashed tables of nibble buckets of all the models, one after another.
struct HashedCounters {
    t: Vec<Bucket>,
    /// log2 of the buckets per model.
    bits: u32,
}

/// Slot 0 of a bucket holds a use count (16 bits), an occupied bit and a check
/// (15 bits). An empty bucket has slot 0 zero. It matches no tag, since every
/// tag has the occupied bit, and with a use count of 0 it is the first to be
/// replaced.
const OCCUPIED: u32 = 1 << 15;

/// Returns an empty bucket: slot 0 zero and every counter at its initial value.
const fn empty_bucket() -> Bucket {
    let mut b = [COUNTER_INIT; 16];
    b[0] = 0;
    Bucket(b)
}

impl HashedCounters {
    fn new(models: usize, buckets_log2: u32) -> Self {
        HashedCounters { t: vec![empty_bucket(); models << buckets_log2], bits: buckets_log2 }
    }

    /// Returns the tag for hash `h`: the occupied bit and a 15-bit check taken
    /// from a remix of `h`, so that it does not depend on the index bits.
    #[inline]
    fn tag(h: u32) -> u32 {
        ((h.wrapping_mul(0x9E37_79B1) >> 17) & (OCCUPIED - 1)) | OCCUPIED
    }

    /// Returns the bucket of `model` for hash `h`, claiming one if there is
    /// none.
    ///
    /// The top bits of `h` choose a pair of adjacent buckets, and the tag tells
    /// which of them, if either, holds this context. On a miss the bucket used
    /// less is reset and taken, so a new context starts from neutral counters.
    /// The other bucket's use count is halved, so a context that was busy once
    /// loses its place in time.
    #[inline]
    fn find(&mut self, model: usize, h: u32) -> usize {
        let i0 = (model << self.bits) | (h >> (32 - self.bits)) as usize;
        let i1 = i0 ^ 1;
        let tag = Self::tag(h);
        for i in [i0, i1] {
            let s0 = self.t[i].0[0];
            if s0 & 0xFFFF == tag {
                let uses = (s0 >> 16).saturating_add(1).min(0xFFFF);
                self.t[i].0[0] = (uses << 16) | tag;
                return i;
            }
        }
        let u0 = self.t[i0].0[0] >> 16;
        let u1 = self.t[i1].0[0] >> 16;
        let victim = if u1 < u0 { i1 } else { i0 };
        let other = victim ^ 1;
        let so = self.t[other].0[0];
        self.t[other].0[0] = ((so >> 17) << 16) | (so & 0xFFFF);
        self.t[victim] = empty_bucket();
        self.t[victim].0[0] = (1 << 16) | tag;
        victim
    }
}

/// log2 of the spacing of an [`Apm`]'s knots in the stretched domain.
const APM_SHIFT: u32 = 8;
/// The knots of one curve, from −2048 to 2048.
const APM_KNOTS: usize = (PSCALE as usize >> APM_SHIFT) + 1;
/// A knot with the whole interpolation weight moves `2^-APM_RATE` of the way
/// toward the bit.
const APM_RATE: u32 = 6;

/// An adaptive probability map: a learned correction of the mixer's output
/// (secondary estimation; Mahoney 2005, §4.2; Knoll and de Freitas 2012,
/// §3.1). For each context it keeps a curve from input probability to output
/// probability. The curve is a row of knots evenly spaced in the stretched
/// domain, which puts them closer together near 0 and 1, and it starts as the
/// identity. The output interpolates between the two knots around the input.
/// After the bit is coded each of those two knots moves toward it, in
/// proportion to its share of the interpolation.
struct Apm {
    t: Vec<u16>,
    /// The lower knot of the last prediction.
    lo: usize,
    /// The weight of the upper knot in the last prediction, of `2^APM_SHIFT`.
    w: i32,
}

impl Apm {
    fn new(contexts: usize) -> Self {
        let curve: Vec<u16> = (0..APM_KNOTS).map(|j| (squash(((j as i32) << APM_SHIFT) - 2048) * 16) as u16).collect();
        Apm { t: curve.repeat(contexts), lo: 0, w: 0 }
    }

    /// Returns the corrected probability for an input whose stretch is `st`, in
    /// context `cx`, and remembers the knots to train.
    #[inline]
    fn p(&mut self, st: i32, cx: usize) -> i32 {
        let s = st + 2048;
        let full = 1 << APM_SHIFT;
        self.w = s & (full - 1);
        self.lo = cx * APM_KNOTS + (s >> APM_SHIFT) as usize;
        let a = i32::from(self.t[self.lo]);
        let b = i32::from(self.t[self.lo + 1]);
        ((a * (full - self.w) + b * self.w) >> (APM_SHIFT + 4)).clamp(1, 4095)
    }

    #[inline]
    fn update(&mut self, bit: bool) {
        let g = if bit { 65535 } else { 0 };
        let full = 1 << APM_SHIFT;
        let by = APM_SHIFT + APM_RATE;
        for (k, w) in [(self.lo, full - self.w), (self.lo + 1, self.w)] {
            let v = i32::from(self.t[k]);
            self.t[k] = (v + (((g - v) * w + (1 << (by - 1))) >> by)) as u16;
        }
    }
}

/// The mixer inputs: order 0, order 1, two for each hashed model (its
/// probability and its bit history's), the match model, and a bias.
const N_INPUTS: usize = 2 * N_HASHED + 4;

const _: () = assert!(N_INPUTS < 32, "mixer dot product must fit in i32");

/// A logistic mixer (Knoll and de Freitas 2012, §3.2). The inputs are
/// predictions in the stretched domain, `xᵢ = st(pᵢ)`, and the output is
/// `squash(Σ wᵢ xᵢ)`. The weights follow the gradient of coding cost: after a
/// bit `y`, `wᵢ += η · xᵢ · (y − p)`. An input that was confident and right
/// gains weight, and one that was confident and wrong loses it. The mixer holds
/// several sets of weights, and a small context chooses the set to use and
/// train, so different models can be trusted in different situations.
struct Mixer {
    w: Vec<i32>,
    /// Offset of the selected weight set in `w`.
    base: usize,
    pr: i32,
}

impl Mixer {
    fn new(sets: usize) -> Self {
        Mixer { w: vec![(1 << 16) / 8; sets * N_INPUTS], base: 0, pr: 2048 }
    }

    /// Mixes the inputs `x` with weight set `set`.
    #[inline]
    fn mix(&mut self, x: &[i32; N_INPUTS], set: usize) -> i32 {
        self.base = set * N_INPUTS;
        let w = &self.w[self.base..self.base + N_INPUTS];
        // |x| < 2^11 and |w >> 8| <= 2^15, so each product is below 2^26
        // and the sum of N_INPUTS (< 32) products fits in an i32.
        let dot: i32 = x.iter().zip(w).map(|(&x, &w)| x * (w >> 8)).sum();
        let d = (dot >> 8).clamp(-2047, 2047);
        self.pr = squash(d);
        self.pr
    }

    /// Trains the weight set last used, on the inputs `x` it mixed and the
    /// coded `bit`.
    #[inline]
    fn update(&mut self, x: &[i32; N_INPUTS], bit: bool) {
        // The error y − p in 12-bit units; the shift by 10 below is the
        // learning rate.
        let err = ((i32::from(bit) << 12) - self.pr) * 2;
        let w = &mut self.w[self.base..self.base + N_INPUTS];
        for (wi, &xi) in w.iter_mut().zip(x) {
            *wi = (*wi + ((xi * err + (1 << 9)) >> 10)).clamp(-(1 << 23), 1 << 23);
        }
    }
}

/// The match model. If the last [`MATCH_MINLEN`] bytes occurred before, the
/// byte that followed them then is a good guess for the next byte now. It
/// reaches back over the whole block, far beyond any context order, and so
/// catches long repeats. Its prediction is a counter chosen by the match length
/// and the expected bit, which lets the mixer learn what a match of each length
/// is worth.
struct MatchModel {
    buf: Vec<u8>,
    pos: usize,
    table: Vec<u32>,
    table_bits: u32,
    ptr: usize,
    len: usize,
    /// Predicted byte, valid while `len > 0`.
    predicted: u8,
    counters: Counters,
    ctx: usize,
}

const MATCH_MINLEN: usize = 6;
/// The match table has one entry for every `2^MATCH_TABLE_SHIFT` bytes of
/// history.
const MATCH_TABLE_SHIFT: u32 = 2;

/// Returns the log2 sizes of the match history, which holds a whole block of
/// `len` bytes, and of its hash table.
fn match_bits(len: usize) -> (u32, u32) {
    let buf_bits = len.max(1 << 16).next_power_of_two().trailing_zeros();
    (buf_bits, (buf_bits - MATCH_TABLE_SHIFT).clamp(16, 24))
}

impl MatchModel {
    /// Returns a match model whose history holds a block of `len` bytes.
    fn new(len: usize) -> Self {
        let (buf_bits, table_bits) = match_bits(len);
        MatchModel {
            buf: vec![0; 1 << buf_bits],
            pos: 0,
            table: vec![0; 1 << table_bits],
            table_bits,
            ptr: 0,
            len: 0,
            predicted: 0,
            counters: Counters::new(64 * 2, 1023),
            ctx: 0,
        }
    }

    fn mask(&self) -> usize {
        self.buf.len() - 1
    }

    /// Advances by one byte. `h` is a hash of the last [`MATCH_MINLEN`] bytes.
    ///
    /// A match that predicted this byte grows by one; any other ends. With no
    /// match, the table gives the last position that followed the same hash,
    /// and that match is taken if at least [`MATCH_MINLEN`] bytes before it
    /// agree, since a hash can collide. Its length is counted backward, up to
    /// 32. The table then records the current position under `h`, so the most
    /// recent occurrence is the one found.
    fn byte_update(&mut self, byte: u8, h: u32) {
        let m = self.mask();
        self.buf[self.pos & m] = byte;
        self.pos += 1;
        if self.len > 0 && self.buf[self.ptr & m] == byte {
            self.len += 1;
            self.ptr += 1;
        } else {
            self.len = 0;
        }
        let hi = (h >> (32 - self.table_bits)) as usize;
        if self.len == 0 && self.pos >= MATCH_MINLEN {
            let cand = self.table[hi] as usize;
            if cand > 0 && self.pos - cand < self.buf.len() {
                let mut l = 0;
                while l < 32 && l < cand && self.buf[(cand - 1 - l) & m] == self.buf[(self.pos - 1 - l) & m] {
                    l += 1;
                }
                if l >= MATCH_MINLEN {
                    self.len = l;
                    self.ptr = cand;
                }
            }
        }
        if self.pos >= MATCH_MINLEN {
            self.table[hi] = self.pos as u32;
        }
        if self.len > 0 {
            self.predicted = self.buf[self.ptr & m];
        }
    }

    /// Returns the match model's input to the mixer for the next bit. `c0` is
    /// the partial byte behind a leading 1, and `nbits` the number of its bits
    /// known.
    ///
    /// While the bits so far agree with the predicted byte, the input is the
    /// next bit of that byte, with the confidence learned for matches of this
    /// length. Once a bit disagrees the match is over and the input is 0, which
    /// is no opinion.
    #[inline]
    fn predict(&mut self, c0: u32, nbits: u32) -> i32 {
        if self.len > 0 {
            let pc = (u32::from(self.predicted) | 0x100) >> (8 - nbits);
            if pc == c0 {
                let expected = (self.predicted >> (7 - nbits)) & 1;
                // Length classes: exact up to 15, then one class per three
                // bytes, up to 31 classes.
                let l = self.len.min(15) + if self.len > 15 { (self.len - 15).min(48) / 3 } else { 0 };
                self.ctx = (l.min(31) << 1 | usize::from(expected)) + 2;
                return stretch(self.counters.p(self.ctx));
            }
            self.len = 0;
        }
        self.ctx = 0;
        0
    }

    #[inline]
    fn update(&mut self, bit: bool) {
        if self.ctx != 0 {
            self.counters.update(self.ctx, bit);
        }
    }

    /// Returns one of four classes of match length, which selects the mixer's
    /// weight set.
    fn len_class(&self) -> usize {
        match self.len {
            0 => 0,
            1..=15 => 1,
            16..=31 => 2,
            _ => 3,
        }
    }
}

/// Gaps kept as possible record lengths.
const GAPS: usize = 8;
/// Record lengths scored at once, besides the default.
const SCORED: usize = 3;
/// The record length until one is found: a machine word.
const DEFAULT_RECORD: usize = 4;
/// The shortest record length sought. Shorter distances are within reach of
/// the other contexts.
const MIN_RECORD: usize = 7;
/// Record lengths are below this.
const MAX_RECORD: usize = 1 << 16;
/// The votes at which a gap gets a model.
const PROMOTE: u32 = 128;
/// The evidence that replaces the record length in use, in 1/256 bit.
const RECORD_EVIDENCE: i32 = 4096 << 8;

/// Returns `log₂ x` in 1/256 bit for `x >= 1`, exact at powers of two and
/// linear between them, which is within 0.09 bit.
fn log2_q8(x: u32) -> i32 {
    let e = 31 - x.leading_zeros();
    ((e << 8) | ((x << (31 - e)) >> 23) & 0xFF) as i32
}

/// An adaptive model of a byte given the byte some distance back: counts of
/// each byte after each context byte.
struct Conditional {
    counts: Vec<u8>,
    rows: [u16; 256],
    /// A row is current if its stamp equals `epoch`. Clearing the model only
    /// starts a new epoch, and a row is emptied when it is next used, since
    /// candidates come and go far more often than their rows are touched.
    stamps: [u32; 256],
    epoch: u32,
}

impl Conditional {
    fn new() -> Self {
        Conditional { counts: vec![0; 1 << 16], rows: [0; 256], stamps: [0; 256], epoch: 0 }
    }

    fn clear(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        if self.epoch == 0 {
            self.stamps = [u32::MAX; 256];
        }
    }

    /// Returns the cost of `byte` after context byte `y`, in 1/256 bit, and
    /// then counts it. The estimate is `(n + 1/16) / (N + 16)` for `n` of the
    /// `N` bytes seen after `y`.
    fn cost(&mut self, y: u8, byte: u8) -> i32 {
        let row = &mut self.counts[usize::from(y) << 8..][..256];
        let total = &mut self.rows[usize::from(y)];
        if self.stamps[usize::from(y)] != self.epoch {
            self.stamps[usize::from(y)] = self.epoch;
            row.fill(0);
            *total = 0;
        }
        let n = row[usize::from(byte)];
        let cost = log2_q8(16 * u32::from(*total) + 256) - log2_q8(16 * u32::from(n) + 1);
        if n == u8::MAX {
            *total = 0;
            for c in row.iter_mut() {
                *c = c.div_ceil(2);
                *total += u16::from(*c);
            }
        }
        row[usize::from(byte)] += 1;
        *total += 1;
        cost
    }
}

/// A gap that may be the record length.
#[derive(Clone, Copy, Default)]
struct Gap {
    len: usize,
    /// Its weight among the gaps seen.
    votes: u32,
    /// The model that scores it, if it has one.
    model: Option<usize>,
    /// How far it has run ahead of the length in use, in 1/256 bit.
    lead: i32,
}

/// Finds the length of fixed-length records.
///
/// A record length is worth having if the byte one record back tells more
/// about the next byte than the byte at the default distance does. Each
/// candidate length gets an adaptive model of a byte given the byte that far
/// back, and the model's running code length is the measure, as in Rissanen's
/// predictive form of the minimum description length principle (1984). The
/// difference in code length between a candidate and the length in use is
/// accumulated by Page's cumulative-sum test (1954), and the candidate
/// replaces the length in use when it has run [`RECORD_EVIDENCE`] ahead.
///
/// The candidates are the commonest gaps between a byte and the previous byte
/// of the same value, kept by the frequent-items algorithm of Misra and Gries
/// (1982). A gap is weighed by the information in the byte, so a run of zeros
/// proposes nothing.
struct Records {
    /// One more than the last position of each byte value, or 0.
    last: [u32; 256],
    /// How often each byte value has occurred, halved now and then.
    freq: [u16; 256],
    total: u32,
    gaps: [Gap; GAPS],
    /// The model of the default length, then those of the scored gaps.
    models: Vec<Conditional>,
    /// The gap in use, or `GAPS` for the default length.
    used: usize,
    /// How far the gap in use has run behind the default length.
    behind: i32,
    /// Models not scoring any gap.
    free: usize,
}

impl Records {
    fn new() -> Self {
        Records {
            last: [0; 256],
            freq: [0; 256],
            total: 0,
            gaps: [Gap::default(); GAPS],
            models: (0..=SCORED).map(|_| Conditional::new()).collect(),
            used: GAPS,
            behind: 0,
            free: SCORED,
        }
    }

    /// Returns the record length in use.
    fn len(&self) -> usize {
        if self.used < GAPS {
            self.gaps[self.used].len
        } else {
            DEFAULT_RECORD
        }
    }

    /// Takes in `byte`, the last of the `pos` bytes in `history`.
    fn update(&mut self, byte: u8, history: &[u8], pos: usize) {
        let b = usize::from(byte);
        let mask = history.len() - 1;
        let back = |len: usize| if len < pos { history[(pos - 1 - len) & mask] } else { 0 };

        // What the byte costs after the byte one record back, for the default
        // length and for each scored gap.
        let default = self.models[0].cost(back(DEFAULT_RECORD), byte);
        let mut costs = [0; GAPS];
        for (k, g) in self.gaps.iter().enumerate() {
            if let Some(m) = g.model {
                costs[k] = self.models[m].cost(back(g.len), byte);
            }
        }
        let held = if self.used < GAPS { costs[self.used] } else { default };
        if self.used < GAPS {
            self.behind = (self.behind + held - default).clamp(0, 1 << 28);
        }
        for (k, g) in self.gaps.iter_mut().enumerate() {
            if g.model.is_some() && k != self.used {
                g.lead = (g.lead + held - costs[k]).clamp(0, 1 << 28);
            }
        }

        // The gap to the previous byte of this value votes for a length, with
        // the weight of the information in the byte, in whole bits.
        let bits = (self.total + 1).leading_zeros().abs_diff((u32::from(self.freq[b]) + 1).leading_zeros());
        let gap = pos - self.last[b] as usize;
        let seen = self.last[b] != 0;
        self.last[b] = pos as u32;
        if seen && (MIN_RECORD..MAX_RECORD).contains(&gap) && bits > 0 {
            self.vote(gap, bits);
        }
        self.freq[b] += 1;
        self.total += 1;
        if self.total == 1 << 14 {
            self.total = 0;
            for f in &mut self.freq {
                *f = f.div_ceil(2);
                self.total += u32::from(*f);
            }
        }

        let mut best = GAPS;
        for (k, g) in self.gaps.iter().enumerate() {
            if g.model.is_some() && k != self.used && (best == GAPS || g.lead > self.gaps[best].lead) {
                best = k;
            }
        }
        if best < GAPS && self.gaps[best].lead >= RECORD_EVIDENCE {
            self.used = best;
        } else if self.used < GAPS && self.behind >= RECORD_EVIDENCE {
            self.used = GAPS;
        } else {
            return;
        }
        self.behind = 0;
        for g in &mut self.gaps {
            g.lead = 0;
        }
    }

    /// Counts `bits` for `gap` among the gaps kept, and gives a free model to
    /// the strongest gap without one.
    fn vote(&mut self, gap: usize, bits: u32) {
        if let Some(g) = self.gaps.iter_mut().find(|g| g.len == gap) {
            g.votes = (g.votes + bits).min(1 << 16);
        } else if let Some(g) = self.gaps.iter_mut().find(|g| g.votes == 0 && g.model.is_none()) {
            *g = Gap { len: gap, votes: bits, model: None, lead: 0 };
        } else {
            for (k, g) in self.gaps.iter_mut().enumerate() {
                g.votes = g.votes.saturating_sub(bits);
                // A scored gap that has lost its votes and leads nothing
                // gives up its model.
                if g.votes == 0 && g.lead == 0 && k != self.used && g.model.take().is_some() {
                    self.free += 1;
                }
            }
        }
        if self.free == 0 {
            return;
        }
        let mut taken = [false; SCORED + 1];
        for g in &self.gaps {
            if let Some(m) = g.model {
                taken[m] = true;
            }
        }
        let Some(free) = (1..=SCORED).find(|&m| !taken[m]) else { return };
        let mut strongest = GAPS;
        for (k, g) in self.gaps.iter().enumerate() {
            if g.model.is_none() && g.votes >= PROMOTE && (strongest == GAPS || g.votes > self.gaps[strongest].votes) {
                strongest = k;
            }
        }
        if strongest < GAPS {
            self.models[free].clear();
            self.gaps[strongest].model = Some(free);
            self.gaps[strongest].lead = 0;
            self.free -= 1;
        }
    }
}

/// Returns a 32-bit hash of two words.
#[inline]
fn hash(a: u32, b: u32) -> u32 {
    let mut h = a.wrapping_mul(0x9E37_79B1) ^ b.wrapping_mul(0x85EB_CA6B).rotate_left(13);
    h ^= h >> 15;
    h = h.wrapping_mul(0xC2B2_AE35);
    h ^ (h >> 13)
}

/// log2 of the buckets of a hashed model, which make 16 MiB.
const BUCKET_BITS: u32 = 18;
/// The hashed models: orders 2, 3, 4 and 6, the word, and two sparse contexts.
const N_HASHED: usize = 7;

/// The context-mixing predictor.
pub struct Cm {
    hmaps: HistMap,
    /// Where in `hmaps` each hashed model's bit history is for this bit.
    hidx: [usize; N_HASHED],
    o0: Counters,
    o1: Counters,
    hashed: HashedCounters,
    /// Bucket of each hashed model for the current nibble.
    bucket: [usize; N_HASHED],
    /// The mixers' inputs for the current bit.
    x: [i32; N_INPUTS],
    /// The context hash of each hashed model for the current byte.
    hctx: [u32; N_HASHED],
    /// Order-1 slot for the current bit (order 0 is indexed by `c0`).
    o1_idx: usize,
    /// Position of the current bit in its nibble's tree, the slot in each
    /// hashed model's bucket.
    node: usize,
    mm: MatchModel,
    mixer: Mixer,
    /// The second mixer, with weights chosen by the previous byte.
    mixer2: Mixer,
    apm1: Apm,
    apm2: Apm,
    /// Partial byte with a leading 1 bit (1..=255).
    c0: u32,
    nbits: u32,
    /// Last four bytes, most recent in the low byte.
    c4: u32,
    /// Bytes 5..8 back.
    c8: u32,
    word: u32,
    records: Records,
    pr: i32,
}

impl Cm {
    /// Returns an upper bound, in bytes, on what [`Cm::new`] allocates for a
    /// block of `len` bytes.
    pub fn memory(len: usize) -> u64 {
        let (buf_bits, table_bits) = match_bits(len);
        let hashed = N_HASHED as u64 * (64u64 << BUCKET_BITS);
        let direct = 4 * (256 + (1 << 16)) as u64;
        let apms = (2 * APM_KNOTS * (256 + (1 << 16))) as u64;
        let mixer = 4 * 5 * 256 * N_INPUTS as u64;
        let tables = N_HASHED as u64 * 4 * 64 * 64;
        let matcher = (1u64 << buf_bits) + (4u64 << table_bits) + 4 * 128;
        let records = ((SCORED + 1) << 16) as u64;
        hashed + direct + apms + mixer + tables + matcher + records + (1 << 16)
    }

    /// Returns a predictor for a block of up to `len` bytes.
    pub fn new(len: usize) -> Self {
        let mut cm = Cm {
            hmaps: HistMap::new(N_HASHED),
            hidx: [0; N_HASHED],
            o0: Counters::new(256, 60),
            o1: Counters::new(1 << 16, 120),
            hashed: HashedCounters::new(N_HASHED, BUCKET_BITS),
            bucket: [0; N_HASHED],
            x: [0; N_INPUTS],
            hctx: [0; N_HASHED],
            o1_idx: 0,
            node: 0,
            mm: MatchModel::new(len),
            mixer: Mixer::new(4 * 256),
            mixer2: Mixer::new(256),
            apm1: Apm::new(256),
            apm2: Apm::new(1 << 16),
            c0: 1,
            nbits: 0,
            c4: 0,
            c8: 0,
            word: 0,
            records: Records::new(),
            pr: 2048,
        };
        cm.set_byte_contexts();
        cm.predict();
        cm
    }

    /// Returns the byte `k` records back, or 0 if the block does not reach
    /// that far. The match history holds the whole block.
    fn above(&self, k: usize) -> u32 {
        let d = k * self.records.len();
        if d > self.mm.pos {
            return 0;
        }
        u32::from(self.mm.buf[(self.mm.pos - d) & (self.mm.buf.len() - 1)])
    }

    /// Computes the context of each hashed model for the next byte: the last 2,
    /// 3, 4 and 6 bytes; the current word (letters only, case folded) with the
    /// previous byte; and two sparse contexts for fixed-width data, described
    /// where they are computed. The second argument of each `hash` keeps the
    /// models' hash spaces apart.
    fn set_byte_contexts(&mut self) {
        let c4 = self.c4;
        let col = self.mm.pos as u32 & 3;
        self.hctx = [
            hash(c4 & 0xFFFF, 2),
            hash(c4 & 0xFF_FFFF, 3),
            hash(c4, 4),
            hash(hash(c4, 6), self.c8 & 0xFFFF),
            hash(self.word, 7 + ((c4 & 0xFF) << 8)),
            // Bytes 2 and 4 back with the position mod 4: the same field of
            // the previous 4-byte word, as in machine instructions.
            hash(c4 & 0xFF00_FF00, 8 + (col << 8)),
            // The bytes one and two records back, with the previous byte.
            hash(self.above(1) | (self.above(2) << 8) | ((c4 & 0xFF) << 16), 10 | ((self.records.len() as u32) << 4)),
        ];
    }

    /// Predicts the next bit into `self.pr`, as the module documentation
    /// describes. The hashed models look up their buckets only at the start of
    /// a nibble, since the four bits of a nibble share a bucket.
    fn predict(&mut self) {
        let c0 = self.c0;
        let c1 = (self.c4 & 0xFF) as usize;
        self.o1_idx = (c1 << 8) | c0 as usize;
        let x = &mut self.x;
        x[0] = stretch(self.o0.p(c0 as usize));
        x[1] = stretch(self.o1.p(self.o1_idx));
        if self.nbits == 0 || self.nbits == 4 {
            for k in 0..N_HASHED {
                self.bucket[k] = self.hashed.find(k, hash(self.hctx[k], c0));
            }
        }
        // The bits of the current nibble seen so far, with a leading 1: the
        // node of the nibble's tree, and so the slot in each bucket.
        self.node = if self.nbits < 4 {
            c0 as usize
        } else {
            let m = self.nbits - 4;
            ((c0 & ((1 << m) - 1)) | (1 << m)) as usize
        };
        for k in 0..N_HASHED {
            let slot = self.hashed.t[self.bucket[k]].0[self.node];
            x[2 + k] = stretch(counter_p(slot));
            let hi = k * HIST_STATES + hist_index(slot);
            self.hidx[k] = hi;
            x[2 + N_HASHED + k] = stretch(self.hmaps.p(hi));
        }
        x[2 * N_HASHED + 2] = self.mm.predict(c0, self.nbits);
        x[2 * N_HASHED + 3] = 256;
        let set = (self.mm.len_class() << 8) | c0 as usize;
        let m1 = self.mixer.mix(x, set);
        let m2 = self.mixer2.mix(x, c1);
        let pm = squash((stretch(m1) + stretch(m2)) >> 1);
        let s = stretch(pm);
        let a1 = self.apm1.p(s, c0 as usize);
        let a2 = self.apm2.p(s, (c1 << 8) | c0 as usize);
        self.pr = ((pm + a1 + 2 * a2 + 2) >> 2).clamp(1, 4095);
    }

    /// Returns the probability that the next bit is 1, as a fraction of 2^16.
    /// It is always in `16..=65520`.
    #[inline]
    pub fn p1(&self) -> u16 {
        (self.pr << 4) as u16
    }

    /// Learns the coded `bit` and predicts the next. Every model, mixer and map
    /// trains the slot or weights it used for the prediction. At the end of a
    /// byte the history, the word, the match model and the record detector
    /// advance, and the contexts are recomputed.
    pub fn update(&mut self, bit: bool) {
        self.o0.update(self.c0 as usize, bit);
        self.o1.update(self.o1_idx, bit);
        for k in 0..N_HASHED {
            hist_update(&mut self.hashed.t[self.bucket[k]].0[self.node], bit);
            self.hmaps.update(self.hidx[k], bit);
        }
        self.mm.update(bit);
        self.mixer.update(&self.x, bit);
        self.mixer2.update(&self.x, bit);
        self.apm1.update(bit);
        self.apm2.update(bit);

        self.c0 = (self.c0 << 1) | u32::from(bit);
        self.nbits += 1;
        if self.nbits == 8 {
            let byte = (self.c0 & 0xFF) as u8;
            self.c8 = (self.c8 << 8) | (self.c4 >> 24);
            self.c4 = (self.c4 << 8) | u32::from(byte);
            self.c0 = 1;
            self.nbits = 0;
            let lower = byte.to_ascii_lowercase();
            if lower.is_ascii_lowercase() {
                self.word = hash(self.word, u32::from(lower));
            } else {
                self.word = 0;
            }
            let h6 = hash(hash(self.c4, 0x6D), self.c8 & 0xFFFF);
            self.mm.byte_update(byte, h6);
            self.records.update(byte, &self.mm.buf, self.mm.pos);
            self.set_byte_contexts();
        }
        self.predict();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The table is the logistic function rounded to the nearest integer.
    #[test]
    fn squash_is_the_logistic_function() {
        for d in -2047..=2047 {
            let exact = 4096.0 / (1.0 + (-f64::from(d) / 256.0).exp());
            let got = f64::from(squash(d));
            assert!((got - exact.clamp(1.0, 4095.0)).abs() <= 0.5 + 1e-6, "d={d} got={got} exact={exact}");
            assert_eq!(squash(d) + squash(-d), 4096, "d={d}");
        }
        assert!((-2047..2047).all(|d| squash(d) <= squash(d + 1)));
    }

    /// No other `d` has a squash nearer `p`, and `stretch` is odd.
    #[test]
    fn stretch_inverts_squash() {
        for p in 1..4096 {
            let d = stretch(p);
            let err = (squash(d) - p).abs();
            assert!(err <= 2, "p={p} d={d}");
            assert!((-2047..=2047).all(|x| (squash(x) - p).abs() >= err), "p={p}");
            assert_eq!(stretch(4096 - p), -d, "p={p}");
        }
        assert_eq!(stretch(2048), 0);
    }

    /// A map starts as the identity and learns a context's bias.
    #[test]
    fn a_probability_map_starts_as_the_identity_and_learns() {
        let mut apm = Apm::new(2);
        for st in (-2048..=2047).step_by(1 << APM_SHIFT) {
            assert!((apm.p(st.max(-2047), 1) - squash(st)).abs() <= 1, "st={st}");
        }
        for _ in 0..2000 {
            apm.p(stretch(2048), 0);
            apm.update(true);
        }
        assert!(apm.p(stretch(2048), 0) > 3900);
        assert!((apm.p(stretch(2048), 1) - 2048).abs() <= 3, "another context is untouched");
    }

    #[test]
    fn empty_buckets_are_never_matched_and_are_claimed_first() {
        let mut t = HashedCounters::new(1, 4);
        let shift = 32 - 4;
        // Fresh buckets are empty: metadata 0, use count 0.
        assert!(t.t.iter().all(|b| b.0[0] == 0 && b.0[1..].iter().all(|&c| c == COUNTER_INIT)));
        // Any hash, including ones whose check bits are zero, claims a
        // bucket with use count 1 rather than matching an empty one.
        for h in [0u32, 0x8000_0000, 0x0000_FFFF, 0xFFFF_0000] {
            let mut t = HashedCounters::new(1, 4);
            let i = t.find(0, h);
            assert_eq!(t.t[i].0[0] >> 16, 1, "h={h:#x}");
            assert_ne!(t.t[i].0[0] & OCCUPIED, 0);
            // The pair's other bucket is still empty.
            assert_eq!(t.t[i ^ 1].0[0], 0);
            // Finding it again is a hit that increments the use count.
            assert_eq!(t.find(0, h), i);
            assert_eq!(t.t[i].0[0] >> 16, 2);
        }
        // A second context mapping to the same pair takes the empty bucket,
        // not the occupied one.
        let h = 0x1234_5678;
        let i = t.find(0, h);
        let pair = |x: u32| ((x >> shift) as usize & 15) & !1;
        let mut other = h;
        while pair(other) != pair(h) || HashedCounters::tag(other) == HashedCounters::tag(h) {
            other = other.wrapping_add(0x9E37_79B9);
        }
        let j = t.find(0, other);
        assert_eq!(j, i ^ 1, "empty bucket in the pair should be claimed first");
        assert_eq!(t.find(0, h), i, "the first context is still present");
    }

    #[test]
    fn probabilities_stay_codable() {
        let mut cm = Cm::new(1 << 16);
        let mut s = 1u64;
        for i in 0..200_000u32 {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1);
            let bit = if i % 3 == 0 { (s >> 40) & 1 == 1 } else { i % 7 < 3 };
            let p = cm.p1();
            assert!((16..=65520).contains(&p));
            cm.update(bit);
        }
    }

    fn feed(cm: &mut Cm, data: &[u8]) {
        for &byte in data {
            for k in (0..8).rev() {
                cm.update((byte >> k) & 1 == 1);
            }
        }
    }

    /// Records of 37 bytes whose columns hold steady values are found, and
    /// the bytes above are those one and two records back.
    #[test]
    fn a_record_length_is_found() {
        const R: usize = 37;
        let mut s = 7u64;
        let data: Vec<u8> = (0..400 * R)
            .map(|i| {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let column = (i % R) as u8;
                if column % 5 == 0 {
                    (s >> 40) as u8
                } else {
                    column.wrapping_mul(29) ^ 0x5A
                }
            })
            .collect();
        let mut cm = Cm::new(data.len());
        assert_eq!(cm.records.len(), DEFAULT_RECORD);
        feed(&mut cm, &data);
        assert_eq!(cm.records.len(), R);
        assert_eq!(cm.above(1), u32::from(data[data.len() - R]));
        assert_eq!(cm.above(2), u32::from(data[data.len() - 2 * R]));
        let mut cm = Cm::new(64);
        feed(&mut cm, b"abc");
        assert_eq!(cm.above(1), 0, "the block does not reach a record back");
    }

    /// The code length of a byte falls as its context is learned.
    #[test]
    fn a_conditional_model_learns() {
        let mut m = Conditional::new();
        let first = m.cost(7, 9);
        for _ in 0..600 {
            m.cost(7, 9);
        }
        assert!(first >= 8 << 8 && m.cost(7, 9) < 64 && m.cost(7, 10) > 8 << 8);
        assert_eq!((log2_q8(1), log2_q8(2), log2_q8(3), log2_q8(1 << 16)), (0, 256, 384, 16 << 8));
    }
}
