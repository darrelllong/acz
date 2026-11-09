// Copyright (c) 2026 Darrell D. E. Long. All rights reserved.
// Use and redistribution are subject to the BSD 3-clause license, with a
// dedication clause, in the LICENSE file at the root of this repository.
//
// Dedicated to the memory of my colleague Glen G. Langdon

//! A context-mixing bit predictor in the lineage of Mahoney's PAQ/lpaq
//! (2002–2007), built entirely from integer arithmetic so that encoder and
//! decoder agree bit-for-bit on every platform.
//!
//! Each byte is coded as 8 binary events, most significant bit first
//! (Langdon's decomposition of a byte into binary decisions). For every bit:
//!
//! 1. *Context models* give probabilities. Orders 0 and 1 are directly
//!    indexed counters. Seven hashed models (orders 2, 3, 4 and 6, the
//!    current word, and two sparse contexts for fixed-width records) each
//!    keep, per context, an adaptive probability and a bit history
//!    (Mahoney 2005) mapped to a probability; both are mixer inputs. One
//!    sparse context switches to the bytes one and two records above once
//!    a record length is detected (after paq8). A *match model* predicts
//!    the bit that followed the most recent occurrence of the last 6 bytes.
//! 2. Two *logistic mixers* combine the inputs in the stretched domain,
//!    `st(p) = ln(p / (1 - p))`: `p = squash(Σ wᵢ · st(pᵢ))`, with weights
//!    trained online by gradient descent on coding cost. The first selects
//!    its weights by match length and partial byte, the second by the
//!    previous byte; their outputs are averaged in the stretched domain.
//! 3. Two *adaptive probability maps* (APM/SSE), in order-0 and order-1
//!    context, refine the result, which is blended as
//!    `(p + apm₀ + 2·apm₁) / 4`.
//!
//! **References.**
//! M. V. Mahoney, *Adaptive Weighing of Context Models for Lossless Data
//! Compression*, Florida Institute of Technology Technical Report
//! CS-2005-16, 2005 (bit counts `n0`, `n1`; the earlier, linear mixer).
//! M. V. Mahoney, *paq8l* (2007) and *lpaq1* (2007), compressor source
//! code, <https://mattmahoney.net/dc/paq8l.zip> and
//! <https://mattmahoney.net/dc/lpaq1.zip> (logistic mixing, adaptive
//! probability maps, the stretch and squash tables, record detection).

/// Probabilities in the stretch/squash domain are 12-bit (0..4096).
const PSCALE: i32 = 4096;

/// `squash(d) = 4096 / (1 + e^{-d/256})`, by interpolation (from lpaq1).
fn squash(d: i32) -> i32 {
    const T: [i32; 33] = [
        1, 2, 3, 6, 10, 16, 27, 45, 73, 120, 194, 310, 488, 747, 1101, 1546, 2047, 2549, 2994, 3348, 3607, 3785, 3901,
        3975, 4022, 4050, 4068, 4079, 4085, 4089, 4092, 4093, 4094,
    ];
    if d > 2047 {
        return 4095;
    }
    if d < -2047 {
        return 1;
    }
    let w = d & 127;
    let i = ((d >> 7) + 16) as usize;
    (T[i] * (128 - w) + T[i + 1] * w + 64) >> 7
}

/// Inverse of [`squash`]: `stretch(p) = 256 · ln(p / (4096 − p))` for a
/// 12-bit `p`, clamped to ±2047.
struct Stretch(Vec<i16>);

impl Stretch {
    /// Built by inverting `squash` rather than evaluating `ln`, so that
    /// `squash(stretch(p))` is as close to `p` as the table allows and the
    /// result is exact integer arithmetic on every platform: walk `x` up
    /// from −2047, and give each `p` from the previous `squash` value up to
    /// `squash(x)` the stretch `x`.
    fn new() -> Self {
        let mut t = vec![0i16; PSCALE as usize];
        let mut pi = 0usize;
        for x in -2047..=2047 {
            let v = squash(x) as usize;
            for slot in t.iter_mut().take(v + 1).skip(pi) {
                *slot = x as i16;
            }
            pi = pi.max(v + 1);
        }
        for slot in t.iter_mut().skip(pi) {
            *slot = 2047;
        }
        Stretch(t)
    }

    #[inline]
    fn get(&self, p12: i32) -> i32 {
        i32::from(self.0[p12 as usize])
    }
}

/// Learning rate after `n` observations, `65536 / (n + 3.5)`. A counter that
/// moves by `1/(n + 3.5)` of the error is close to the running frequency of
/// what it has seen, so it learns fast when new and settles as evidence
/// accumulates; the 3.5 keeps the first update from jumping all the way to
/// 0 or 1 on a single observation.
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

/// A counter's probability, to the 12 bits the mixer uses.
#[inline]
fn counter_p(slot: u32) -> i32 {
    (slot >> 20) as i32
}

/// Move the counter toward the observed bit by `1/(n + 3.5)` of the error
/// (see [`RECIP`]). Capping the count at `limit` keeps the step from
/// shrinking forever, so the counter can still follow data that changes:
/// a small limit tracks recent statistics, a large one averages longer.
#[inline]
fn counter_update(slot: &mut u32, bit: bool, limit: u32) {
    let p = i64::from(*slot >> 16);
    let n = *slot & 0xFFFF;
    let target = if bit { 65535 } else { 0 };
    let np = p + (((target - p) * i64::from(RECIP[n as usize])) >> 16);
    let nn = if n < limit { n + 1 } else { n };
    *slot = ((np as u32) << 16) | nn;
}

/// Cap on each bit-history count.
const HIST_CAP: u32 = 63;
const _: () = assert!(2 * HIST_CAP < 1024, "n0 + n1 must index the reciprocal table");

// A slot of a hashed model holds two things about its context: a 16-bit
// adaptive probability (high half) and a *bit history*, the counts `n0`
// (bits 8..16) and `n1` (bits 0..8) of zeros and ones seen (Mahoney 2005,
// §3). A probability alone cannot tell "seen once" from "seen a thousand
// times with the same result", and the mixer needs to know the difference;
// the bit history says how much evidence there is and how consistent it
// is, and a learned map (`HistMap`) turns it into a second prediction.

/// Index of a slot's bit history `(n0, n1)` in its model's `HistMap`.
#[inline]
fn hist_index(slot: u32) -> usize {
    let n0 = (slot >> 8) & 0xFF;
    let n1 = slot & 0xFF;
    (n0 * (HIST_CAP + 1) + n1) as usize
}

/// Learn `bit` in a slot: move its probability as a counter does, using the
/// evidence so far (`n0 + n1`) as the count; then update the bit history.
/// The history is *semi-stationary*: a 1 adds to `n1` and, if `n0 > 2`,
/// cuts `n0` to `n0 / 2 + 1` (and the mirror image for a 0). Old evidence
/// against the latest outcome is discounted but not forgotten, so a
/// context that changes its behavior is believed again within a few bits.
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

/// What a bit history `(n0, n1)` actually predicts, learned online. Each
/// model has its own map, because "three ones, no zeros" means something
/// different in an order-2 context than in an order-6 one.
struct HistMap {
    t: Vec<u32>,
}

impl HistMap {
    fn new() -> Self {
        let k = HIST_CAP + 1;
        let mut t = vec![0u32; (k * k) as usize];
        for n0 in 0..k {
            for n1 in 0..k {
                // Prior (n1 + ½) / (n0 + n1 + 1), as a 16-bit probability.
                let p = ((2 * n1 + 1) << 16) / (2 * (n0 + n1) + 2);
                t[(n0 * k + n1) as usize] = p.min(65535) << 16;
            }
        }
        HistMap { t }
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

/// A table of counters indexed directly (no hashing).
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

/// One cache line of counters for one hashed context and one nibble: slot
/// 0 is metadata, and slots 1..=15 are the 15 nodes of the binary tree over
/// the nibble's four bits (the node is the bits seen so far, with a leading
/// 1). The context is looked up once per nibble, not once per bit, and the
/// counters for all four bits share one line, so they cost at most one
/// memory access between them.
#[repr(align(64))]
#[derive(Clone, Copy)]
struct Bucket([u32; 16]);

/// A hashed table of nibble buckets.
struct HashedCounters {
    t: Vec<Bucket>,
}

/// Slot 0 of a bucket: `use count (16 bits) | occupied (1 bit) | check (15
/// bits)`. An empty bucket is all zeros: it matches no tag (every tag has
/// the occupied bit set) and, with a use count of 0, is the first victim.
const OCCUPIED: u32 = 1 << 15;

/// A fresh bucket: empty metadata, all counters at their initial value.
const fn empty_bucket() -> Bucket {
    let mut b = [COUNTER_INIT; 16];
    b[0] = 0;
    Bucket(b)
}

impl HashedCounters {
    fn new(buckets_log2: u32) -> Self {
        HashedCounters { t: vec![empty_bucket(); 1 << buckets_log2] }
    }

    /// Tag for hash `h`: a 15-bit check drawn from a remix of `h` (so it is
    /// independent of the index bits) plus the occupied bit.
    #[inline]
    fn tag(h: u32) -> u32 {
        ((h.wrapping_mul(0x9E37_79B1) >> 17) & (OCCUPIED - 1)) | OCCUPIED
    }

    /// Find the bucket for hash `h`, or claim one for it.
    ///
    /// The index (the top bits of `h`) chooses a pair of adjacent buckets,
    /// and the tag tells which, if either, holds this context. A table of
    /// fixed size must forget something when it is full, so on a miss the
    /// bucket used less is reset and taken (an empty one, with use count 0,
    /// first): a new context starts from neutral counters instead of
    /// inheriting a stranger's. The other bucket's use count is halved, so a
    /// context that was busy once but is no longer needed loses its place
    /// over time instead of holding it forever.
    #[inline]
    fn find(&mut self, h: u32, shift: u32) -> usize {
        let mask = self.t.len() - 1;
        let i0 = (h >> shift) as usize & mask;
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

/// APM learning rate: each update moves a bucket 2^-7 of the way.
const APM_RATE: u32 = 7;

/// Adaptive probability map (secondary symbol estimation): a learned
/// correction of the mixer's output. For each small context it keeps a
/// curve from input probability to output probability, sampled at 33
/// points evenly spaced in the stretched domain and starting as the
/// identity. The output interpolates between the two points either side of
/// the input; learning moves the nearer one toward the observed bit. It
/// catches what the mixer gets systematically wrong in that context, such
/// as being too confident or not confident enough.
struct Apm {
    t: Vec<u16>,
    idx: usize,
    rate: u32,
}

impl Apm {
    fn new(contexts: usize, rate: u32) -> Self {
        let mut t = vec![0u16; contexts * 33];
        for c in 0..contexts {
            for j in 0..33 {
                t[c * 33 + j] = (squash((j as i32 - 16) * 128) * 16) as u16;
            }
        }
        Apm { t, idx: 0, rate }
    }

    /// The corrected probability for an input whose stretch is `st`, in
    /// context `cx`; remembers which point to train.
    #[inline]
    fn p(&mut self, st: i32, cx: usize) -> i32 {
        // Place st on the 32 intervals between the 33 points: lo is the
        // interval, w (0..4096) the position within it.
        let s = (st + 2048) * 32;
        let lo = (s >> 12) as usize;
        let w = s & 4095;
        let base = cx * 33 + lo;
        self.idx = base + usize::from(w >= 2048);
        let a = i32::from(self.t[base]);
        let b = i32::from(self.t[base + 1]);
        ((a * (4096 - w) + b * w) >> 16).clamp(1, 4095)
    }

    #[inline]
    fn update(&mut self, bit: bool) {
        let g = if bit { 65535i32 } else { 0 };
        let v = i32::from(self.t[self.idx]);
        self.t[self.idx] = (v + ((g - v) >> self.rate)) as u16;
    }
}

/// Mixer inputs: order 0, order 1, two per hashed model (adaptive
/// probability and bit-history map), match, bias.
const N_INPUTS: usize = 2 * N_HASHED + 4;

const _: () = assert!(N_INPUTS < 32, "mixer dot product must fit in i32");

/// Logistic mixing (paq8l, lpaq1): the inputs are predictions in the
/// stretched domain, `xᵢ = st(pᵢ)`, and the output is `squash(Σ wᵢ xᵢ)`. The
/// weights are learned by gradient descent on coding cost: after a bit `y`,
/// `wᵢ += η · xᵢ · (y − p)`, the gradient of `−log p(y)` for the ideal
/// logistic model (here `squash`, `stretch` and the update are integer
/// approximations of it). A prediction that was confident and right gains
/// weight, one that was confident and wrong loses it. The mixer keeps several sets
/// of weights and a small context chooses which set to use and train, so
/// that it can trust different models in different situations.
struct Mixer {
    w: Vec<i32>,
    x: [i32; N_INPUTS],
    /// Offset of the selected weight set in `w`.
    base: usize,
    pr: i32,
}

impl Mixer {
    fn new(sets: usize) -> Self {
        Mixer { w: vec![(1 << 16) / 8; sets * N_INPUTS], x: [0; N_INPUTS], base: 0, pr: 2048 }
    }

    /// Mix the inputs in `x` with weight set `set`.
    #[inline]
    fn mix(&mut self, set: usize) -> i32 {
        self.base = set * N_INPUTS;
        let w = &self.w[self.base..self.base + N_INPUTS];
        // |x| < 2^11 and |w >> 8| <= 2^15, so each product is below 2^26
        // and the sum of N_INPUTS (< 32) products fits in an i32.
        let dot: i32 = self.x.iter().zip(w).map(|(&x, &w)| x * (w >> 8)).sum();
        let d = (dot >> 8).clamp(-2047, 2047);
        self.pr = squash(d);
        self.pr
    }

    /// Train the weight set last used, after the coded `bit`.
    #[inline]
    fn update(&mut self, bit: bool) {
        // The error y − p in 12-bit units; the shift by 10 below is the
        // learning rate.
        let err = ((i32::from(bit) << 12) - self.pr) * 2;
        let w = &mut self.w[self.base..self.base + N_INPUTS];
        for (wi, &xi) in w.iter_mut().zip(&self.x) {
            *wi = (*wi + ((xi * err + (1 << 9)) >> 10)).clamp(-(1 << 23), 1 << 23);
        }
    }
}

/// The match model: if the last `MATCH_MINLEN` bytes occurred before, the
/// byte that followed them then is a good guess for the next byte now. It
/// reaches back over the whole block, far beyond any context order, which
/// is what catches long repeats. Its prediction is a counter chosen by the
/// match length and the expected bit, so the mixer learns how much a match
/// of each length is worth.
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
/// Match-table size relative to the history buffer (log2 ratio).
const MATCH_TABLE_SHIFT: u32 = 2;

/// log2 sizes of the match history (holding a whole block of `len` bytes)
/// and of its hash table.
fn match_bits(len: usize) -> (u32, u32) {
    let buf_bits = len.max(1 << 16).next_power_of_two().trailing_zeros();
    (buf_bits, (buf_bits - MATCH_TABLE_SHIFT).clamp(16, 24))
}

impl MatchModel {
    /// History sized to hold `len` bytes (a whole block), so a match can
    /// reach back to the start of the block.
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

    /// Advance by one byte. `h` is a hash of the last `MATCH_MINLEN` bytes.
    ///
    /// A current match that predicted this byte grows by one; otherwise it
    /// ends. Without a match, the table gives the last position after the
    /// same hash, and the match is taken if the bytes before it really
    /// agree for at least `MATCH_MINLEN` (a hash can collide). Its length
    /// is counted backward, up to 32. The table then records the current
    /// position under `h`, so the most recent occurrence is always the one
    /// found.
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

    /// The match model's input to the mixer for the next bit; `c0` is the
    /// partial byte with a leading 1, `nbits` the bits of it known.
    ///
    /// While the bits so far agree with the predicted byte, the prediction
    /// is the next bit of that byte, with the confidence learned for
    /// matches of this length; once a bit disagrees the match is over (for
    /// the rest of the byte too) and the input is 0, no opinion.
    #[inline]
    fn predict(&mut self, c0: u32, nbits: u32, st: &Stretch) -> i32 {
        if self.len > 0 {
            let pc = (u32::from(self.predicted) | 0x100) >> (8 - nbits);
            if pc == c0 {
                let expected = (self.predicted >> (7 - nbits)) & 1;
                // Length classes: exact up to 15, then one class per three
                // bytes, up to 31 classes.
                let l = self.len.min(15) + if self.len > 15 { (self.len - 15).min(48) / 3 } else { 0 };
                self.ctx = (l.min(31) << 1 | usize::from(expected)) + 2;
                return st.get(self.counters.p(self.ctx));
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

    /// One of four match-length classes selecting the mixer's weight set.
    fn len_class(&self) -> usize {
        match self.len {
            0 => 0,
            1..=15 => 1,
            16..=31 => 2,
            _ => 3,
        }
    }
}

/// Combine two words into a well-mixed 32-bit hash.
#[inline]
fn hash(a: u32, b: u32) -> u32 {
    let mut h = a.wrapping_mul(0x9E37_79B1) ^ b.wrapping_mul(0x85EB_CA6B).rotate_left(13);
    h ^= h >> 15;
    h = h.wrapping_mul(0xC2B2_AE35);
    h ^ (h >> 13)
}

/// log2 of the number of 64-byte buckets per hashed model (16 MiB each).
const BUCKET_BITS: u32 = 18;
/// Number of hashed context models (orders 2, 3, 4, 6, word, two sparse).
const N_HASHED: usize = 7;

/// The full context-mixing predictor.
pub struct Cm {
    stretch: Stretch,
    /// One bit-history map per hashed model.
    hmaps: Vec<HistMap>,
    /// Bit-history index used by each hashed model for the current bit.
    hidx: [usize; N_HASHED],
    o0: Counters,
    o1: Counters,
    hashed: Vec<HashedCounters>,
    /// Bucket of each hashed model for the current nibble.
    bucket: [usize; N_HASHED],
    /// Per-byte context hashes of the hashed models.
    hctx: [u32; N_HASHED],
    /// Order-1 slot for the current bit (order 0 is indexed by `c0`).
    o1_idx: usize,
    /// Position of the current bit in its nibble's tree, the slot in each
    /// hashed model's bucket.
    node: usize,
    mm: MatchModel,
    mixer: Mixer,
    /// Second mixer, weights selected by the previous byte (PAQ5-style
    /// multiple mixers); the two are averaged in the stretched domain.
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
    /// Record-length detector (paq8 style): last positions of each byte.
    cpos: Box<[[usize; 4]; 256]>,
    rlen: usize,
    rlen1: usize,
    rcount1: u32,
    rlen2: usize,
    rcount2: u32,
    pr: i32,
}

impl Cm {
    /// Upper bound, in bytes, on the memory [`Cm::new`] allocates for a
    /// block of `len` bytes.
    pub fn memory(len: usize) -> u64 {
        let (buf_bits, table_bits) = match_bits(len);
        let hashed = N_HASHED as u64 * (64u64 << BUCKET_BITS);
        let direct = 4 * (256 + (1 << 16)) as u64;
        let apms = 2 * 33 * (256 + (1 << 16)) as u64;
        let mixer = 4 * 5 * 256 * N_INPUTS as u64;
        let tables = 2 * PSCALE as u64 + 4 * 1024 + N_HASHED as u64 * 4 * 64 * 64;
        let matcher = (1u64 << buf_bits) + (4u64 << table_bits) + 4 * 128;
        let record = (256 * 4 * std::mem::size_of::<usize>()) as u64;
        hashed + direct + apms + mixer + tables + matcher + record + (1 << 16)
    }

    /// Fresh predictor for a block of up to `len` bytes (see
    /// [`Cm::memory`] for its size).
    pub fn new(len: usize) -> Self {
        let mut cm = Cm {
            stretch: Stretch::new(),
            hmaps: (0..N_HASHED).map(|_| HistMap::new()).collect(),
            hidx: [0; N_HASHED],
            o0: Counters::new(256, 60),
            o1: Counters::new(1 << 16, 120),
            hashed: (0..N_HASHED).map(|_| HashedCounters::new(BUCKET_BITS)).collect(),
            bucket: [0; N_HASHED],
            hctx: [0; N_HASHED],
            o1_idx: 0,
            node: 0,
            mm: MatchModel::new(len),
            mixer: Mixer::new(4 * 256),
            mixer2: Mixer::new(256),
            apm1: Apm::new(256, APM_RATE),
            apm2: Apm::new(1 << 16, APM_RATE),
            c0: 1,
            nbits: 0,
            c4: 0,
            c8: 0,
            word: 0,
            cpos: Box::new([[0; 4]; 256]),
            rlen: 0,
            rlen1: 0,
            rcount1: 0,
            rlen2: 0,
            rcount2: 0,
            pr: 2048,
        };
        cm.set_byte_contexts();
        cm.predict();
        cm
    }

    /// The byte `k` records back, i.e. in the same column of an earlier
    /// record (0 when no record length is known or it is before the start).
    /// The match buffer holds the whole block, so any distance up to `pos`
    /// is still there.
    fn above(&self, k: usize) -> u32 {
        let d = k * self.rlen;
        if self.rlen == 0 || d > self.mm.pos {
            return 0;
        }
        u32::from(self.mm.buf[(self.mm.pos - d) & (self.mm.buf.len() - 1)])
    }

    /// Detect fixed-length records, after Mahoney's paq8.
    ///
    /// In a table of records, a given byte value tends to recur at the same
    /// distance, the record length. So for each byte value the last four
    /// positions are kept, and when the gap to this occurrence equals the
    /// three gaps before it, that gap `r` is a candidate length. Two
    /// candidates are tracked with confirmation counts (a new one replaces
    /// the weaker); a candidate confirmed 16 times becomes the record length
    /// `rlen`. Adopting one clears both counts, so the detector follows the
    /// data when the record length changes rather than holding on to an old
    /// one.
    fn detect_record(&mut self, byte: u8) {
        let pos = self.mm.pos;
        let c = &mut self.cpos[byte as usize];
        let r = pos - c[0];
        if r > 1 && c[3] > 0 && r == c[0] - c[1] && r == c[1] - c[2] && r == c[2] - c[3] {
            if r == self.rlen1 {
                self.rcount1 += 1;
            } else if r == self.rlen2 {
                self.rcount2 += 1;
            } else if self.rcount1 > self.rcount2 {
                self.rlen2 = r;
                self.rcount2 = 1;
            } else {
                self.rlen1 = r;
                self.rcount1 = 1;
            }
        }
        c[3] = c[2];
        c[2] = c[1];
        c[1] = c[0];
        c[0] = pos;
        if self.rcount1 > 15 && self.rlen != self.rlen1 {
            self.rlen = self.rlen1;
            self.rcount1 = 0;
            self.rcount2 = 0;
        }
        if self.rcount2 > 15 && self.rlen != self.rlen2 {
            self.rlen = self.rlen2;
            self.rcount1 = 0;
            self.rcount2 = 0;
        }
    }

    /// Compute, once per byte, the context of each hashed model: orders 2,
    /// 3, 4 and 6 (the last 2, 3, 4 and 6 bytes), the current word (letters
    /// only, case folded, with the previous byte), and two sparse contexts
    /// for fixed-width binary data (described below). The second argument
    /// of each `hash` only keeps the models' hash spaces apart.
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
            // Once a record length is known: the bytes one and two records
            // above, with the previous byte (the same column of earlier
            // records predicts this one). Until then: bytes 4 and 5 back
            // with the position mod 4.
            if self.rlen > 0 {
                hash(self.above(1) | (self.above(2) << 8) | ((c4 & 0xFF) << 16), 10 | ((self.rlen as u32) << 4))
            } else {
                hash((c4 >> 24) | ((self.c8 & 0xFF) << 8) | (col << 16), 9)
            },
        ];
    }

    /// Predict the next bit into `self.pr`.
    ///
    /// Each model gives its prediction as a stretched probability: order 0
    /// and order 1 from counters indexed by the partial byte, each hashed
    /// model twice (its slot's probability, and what its bit history
    /// predicts), the match model, and a constant bias. The hashed models
    /// look their buckets up only at the start of each nibble, since the
    /// four bits of a nibble share one bucket. Two mixers combine the
    /// inputs, one choosing its weights by match length and partial byte,
    /// the other by the previous byte, and their outputs are averaged. Two
    /// APMs correct the result, in order-0 and order-1 context, and the
    /// final probability blends the mixed and the corrected predictions,
    /// weighting the order-1 correction double.
    fn predict(&mut self) {
        let c0 = self.c0;
        let st = &self.stretch;
        let c1 = (self.c4 & 0xFF) as usize;
        self.o1_idx = (c1 << 8) | c0 as usize;
        let mut x = [0i32; N_INPUTS];
        x[0] = st.get(self.o0.p(c0 as usize));
        x[1] = st.get(self.o1.p(self.o1_idx));
        if self.nbits == 0 || self.nbits == 4 {
            let shift = 32 - BUCKET_BITS;
            for k in 0..N_HASHED {
                self.bucket[k] = self.hashed[k].find(hash(self.hctx[k], c0), shift);
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
            let slot = self.hashed[k].t[self.bucket[k]].0[self.node];
            x[2 + k] = st.get(counter_p(slot));
            let hi = hist_index(slot);
            self.hidx[k] = hi;
            x[2 + N_HASHED + k] = st.get(self.hmaps[k].p(hi));
        }
        x[2 * N_HASHED + 2] = self.mm.predict(c0, self.nbits, st);
        x[2 * N_HASHED + 3] = 256;
        self.mixer.x = x;
        self.mixer2.x = x;
        let set = (self.mm.len_class() << 8) | c0 as usize;
        let m1 = self.mixer.mix(set);
        let m2 = self.mixer2.mix(c1);
        let pm = squash((st.get(m1) + st.get(m2)) >> 1);
        let s = st.get(pm);
        let a1 = self.apm1.p(s, c0 as usize);
        let a2 = self.apm2.p(s, (c1 << 8) | c0 as usize);
        self.pr = ((pm + a1 + 2 * a2 + 2) >> 2).clamp(1, 4095);
    }

    /// Probability that the next bit is 1, as a fraction of 2^16
    /// (always in `16..=65520`).
    #[inline]
    pub fn p1(&self) -> u16 {
        (self.pr << 4) as u16
    }

    /// Learn the coded `bit` in every model, mixer and APM (each trains the
    /// slot or weights it used for the prediction), extend the partial byte,
    /// and predict the next bit. At the end of a byte, also update the byte
    /// history, the word, the match model and the record detector, and
    /// recompute the contexts.
    pub fn update(&mut self, bit: bool) {
        self.o0.update(self.c0 as usize, bit);
        self.o1.update(self.o1_idx, bit);
        for k in 0..N_HASHED {
            let h = &mut self.hashed[k];
            hist_update(&mut h.t[self.bucket[k]].0[self.node], bit);
            self.hmaps[k].update(self.hidx[k], bit);
        }
        self.mm.update(bit);
        self.mixer.update(bit);
        self.mixer2.update(bit);
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
            self.detect_record(byte);
            self.set_byte_contexts();
        }
        self.predict();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stretch_inverts_squash() {
        let st = Stretch::new();
        for p in 1..4095 {
            let d = st.get(p);
            assert!((squash(d) - p).abs() <= 40, "p={p} d={d} squash={}", squash(d));
        }
        assert!(st.get(2048).abs() <= 2);
    }

    #[test]
    fn empty_buckets_are_never_matched_and_are_claimed_first() {
        let mut t = HashedCounters::new(4);
        let shift = 32 - 4;
        // Fresh buckets are empty: metadata 0, use count 0.
        assert!(t.t.iter().all(|b| b.0[0] == 0 && b.0[1..].iter().all(|&c| c == COUNTER_INIT)));
        // Any hash, including ones whose check bits are zero, claims a
        // bucket with use count 1 rather than matching an empty one.
        for h in [0u32, 0x8000_0000, 0x0000_FFFF, 0xFFFF_0000] {
            let mut t = HashedCounters::new(4);
            let i = t.find(h, shift);
            assert_eq!(t.t[i].0[0] >> 16, 1, "h={h:#x}");
            assert_ne!(t.t[i].0[0] & OCCUPIED, 0);
            // The pair's other bucket is still empty.
            assert_eq!(t.t[i ^ 1].0[0], 0);
            // Finding it again is a hit that increments the use count.
            assert_eq!(t.find(h, shift), i);
            assert_eq!(t.t[i].0[0] >> 16, 2);
        }
        // A second context mapping to the same pair takes the empty bucket,
        // not the occupied one.
        let h = 0x1234_5678;
        let i = t.find(h, shift);
        let pair = |x: u32| ((x >> shift) as usize & 15) & !1;
        let mut other = h;
        while pair(other) != pair(h) || HashedCounters::tag(other) == HashedCounters::tag(h) {
            other = other.wrapping_add(0x9E37_79B9);
        }
        let j = t.find(other, shift);
        assert_eq!(j, i ^ 1, "empty bucket in the pair should be claimed first");
        assert_eq!(t.find(h, shift), i, "the first context is still present");
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

    fn feed(cm: &mut Cm, byte: u8) {
        for k in (0..8).rev() {
            cm.update((byte >> k) & 1 == 1);
        }
    }

    #[test]
    fn record_length_is_adopted_on_the_sixteenth_confirmation() {
        // Every byte value recurs with period 37, so from the fifth
        // occurrence of a value on, each byte confirms the gap once.
        const P: usize = 37;
        let data: Vec<u8> = (0..400).map(|i| (i % P) as u8 * 3).collect();
        let mut cm = Cm::new(data.len());
        let first_confirm = 4 * P;
        for (i, &b) in data.iter().enumerate() {
            feed(&mut cm, b);
            if i < first_confirm + 15 {
                assert_eq!(cm.rlen, 0, "adopted early at byte {i}");
            } else if i == first_confirm + 15 {
                assert_eq!(cm.rlen, P, "not adopted at byte {i}");
                assert_eq!((cm.rcount1, cm.rcount2), (0, 0), "counts not cleared");
            }
        }
        assert_eq!(cm.rlen, P);
        let pos = data.len();
        assert_eq!(cm.above(1), u32::from(data[pos - P]));
        assert_eq!(cm.above(2), u32::from(data[pos - 2 * P]));
    }

    #[test]
    fn above_needs_enough_history() {
        let mut cm = Cm::new(1 << 16);
        for b in [5u8, 6, 7] {
            feed(&mut cm, b);
        }
        assert_eq!(cm.above(1), 0, "no record length yet");
        cm.rlen = 3;
        assert_eq!(cm.above(1), 5, "distance equal to pos reaches the first byte");
        assert_eq!(cm.above(2), 0, "distance beyond pos");
    }
}
