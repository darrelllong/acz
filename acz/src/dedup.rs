// Copyright (c) 2026 Darrell D. E. Long. All rights reserved.
// Use and redistribution are subject to the BSD 3-clause license, with a
// dedication clause, in the LICENSE file at the root of this repository.
//
// Dedicated to the memory of my colleague Glen G. Langdon

//! Long-repeat removal ("self-delta") before block compression.
//!
//! Each block (frame) of the input is split into a *residual* (the bytes
//! not covered by copies) and a list of *commands* `(literal run, copy
//! length, distance)`, where every copy repeats bytes that occur earlier in
//! the input, at most `window` bytes back. The residual is then compressed
//! by the block model as usual, so blocks stay independent and parallel;
//! the commands are applied, in order, by the decoder against a bounded
//! history of the reconstructed output.
//!
//! Matching follows the approach of Ajtai, Burns, Fagin, Long and
//! Stockmeyer, *Compactly Encoding Unstructured Inputs with Differential
//! Compression*, JACM 49(3), 2002: Karp–Rabin fingerprints of fixed-length
//! seeds, a fixed-size table of seed positions, and matches extended forward
//! and backward from a seed hit. Here the reference is the input's own
//! history (a causal self-reference), and a seed is a candidate only at
//! content-defined anchor positions, so repeated content produces the same
//! anchors wherever it occurs.

use std::io;

use arith::{BitModel, RangeDecoder, RangeEncoder};

use crate::models::Bounded;

/// Seed (fingerprint window) length.
const SEED: usize = 32;
/// About one position in 2^ANCHOR_BITS is an anchor.
const ANCHOR_BITS: u32 = 5;
/// log2 of the number of anchor-table slots (8 bytes each).
const TABLE_BITS: u32 = 22;
/// An anchor-table slot that holds no position. It is never below a
/// current position, so it never passes the candidate test.
const EMPTY: u64 = u64::MAX;
/// Fingerprint base (odd, 64-bit wrapping arithmetic).
const BASE: u64 = 0x0000_0100_0000_01B3;

/// Minimum copy length. It also bounds the command list (see
/// [`Deduper::frame`]).
pub const MIN_COPY: usize = 256;

fn corrupt(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("corrupt dedup frame: {msg}"))
}

/// Longest LEB128 encoding of a `u64`.
pub(crate) const MAX_VARINT_LEN: usize = 10;

/// Appends `v` as an unsigned LEB128 varint, the variable-length integer of the
/// DWARF debugging format: seven bits to a byte, least significant first, with
/// the top bit set on every byte but the last.
pub(crate) fn put_varint(mut v: u64, out: &mut Vec<u8>) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// Reads a canonical unsigned LEB128 varint: at most [`MAX_VARINT_LEN`] bytes,
/// no bits beyond 64, and no overlong encoding (a final zero byte after the
/// first).
pub(crate) fn get_varint(buf: &[u8], pos: &mut usize) -> io::Result<u64> {
    let mut v = 0u64;
    for i in 0..MAX_VARINT_LEN {
        let b = *buf.get(*pos).ok_or_else(|| corrupt("varint truncated"))?;
        *pos += 1;
        if i == MAX_VARINT_LEN - 1 && b > 1 {
            return Err(corrupt("varint overflows 64 bits"));
        }
        v |= u64::from(b & 0x7F) << (7 * i);
        if b < 0x80 {
            if i > 0 && b == 0 {
                return Err(corrupt("overlong varint"));
            }
            return Ok(v);
        }
    }
    Err(corrupt("varint too long"))
}

/// A ring holding the last `capacity` bytes of the (original) stream, with
/// absolute positions.
pub struct History {
    buf: Vec<u8>,
    mask: usize,
    /// Number of bytes ever appended.
    total: u64,
}

impl History {
    /// `capacity` must be a power of two.
    pub fn new(capacity: usize) -> Self {
        assert!(capacity.is_power_of_two());
        History { buf: vec![0; capacity], mask: capacity - 1, total: 0 }
    }

    fn capacity(&self) -> usize {
        self.buf.len()
    }

    #[inline]
    fn at(&self, abs: u64) -> u8 {
        self.buf[abs as usize & self.mask]
    }

    /// Oldest absolute position still held.
    #[inline]
    fn oldest(&self) -> u64 {
        self.total.saturating_sub(self.capacity() as u64)
    }

    #[inline]
    fn push(&mut self, b: u8) {
        let i = self.total as usize & self.mask;
        self.buf[i] = b;
        self.total += 1;
    }
}

/// Encoder side: finds copies in each frame against everything still in
/// the window (including earlier parts of the same frame).
pub struct Deduper {
    /// The window: the largest copy distance, and the bytes before the
    /// current frame that a copy may read.
    hist: History,
    /// Absolute position of the newest anchor with each fingerprint slot.
    table: Vec<u64>,
    base_pow: u64,
}

impl Deduper {
    /// `window` is the largest copy distance and must be a power of two at
    /// least twice the largest frame.
    pub fn new(window: usize) -> Self {
        let mut base_pow = 1u64;
        for _ in 0..SEED - 1 {
            base_pow = base_pow.wrapping_mul(BASE);
        }
        Deduper { hist: History::new(window), table: vec![EMPTY; 1 << TABLE_BITS], base_pow }
    }

    /// Bytes allocated by a deduper with this window.
    pub fn memory(window: usize) -> u64 {
        window as u64 + (8u64 << TABLE_BITS)
    }

    /// The slot comes from the top bits of the fingerprint and the anchor
    /// test from bits 24 to 28, so the two are independent: anchors are
    /// spread over the whole table.
    #[inline]
    fn slot(h: u64) -> usize {
        (h >> (64 - TABLE_BITS)) as usize
    }

    #[inline]
    fn is_anchor(h: u64) -> bool {
        (h >> 24) & ((1 << ANCHOR_BITS) - 1) == 0
    }

    /// Fingerprint of the seed at the start of `s`: the Karp–Rabin (R. M.
    /// Karp and M. O. Rabin, *Efficient Randomized Pattern-Matching
    /// Algorithms*, IBM J. Res. Develop. 31(2), 1987, pp. 249–260)
    /// polynomial in `BASE`, modulo 2^64. Each byte counts as `b + 1`, so a
    /// zero byte still contributes and an all-zero seed does not hash to 0.
    fn fingerprint(s: &[u8]) -> u64 {
        s[..SEED].iter().fold(0u64, |h, &b| h.wrapping_mul(BASE).wrapping_add(u64::from(b) + 1))
    }

    /// Slides fingerprint `h` one byte: `out` leaves and `inc` enters.
    #[inline]
    fn roll(&self, h: u64, out: u8, inc: u8) -> u64 {
        h.wrapping_sub(self.base_pow.wrapping_mul(u64::from(out) + 1))
            .wrapping_mul(BASE)
            .wrapping_add(u64::from(inc) + 1)
    }

    /// Splits `frame` into (residual, commands). The commands are varints:
    /// a `(literal run, copy length, distance)` triple per copy, then the
    /// length of the final literal run.
    ///
    /// The fingerprint of the seed at `p` is rolled forward one byte at a
    /// time. At an anchor, the table gives the newest earlier anchor with
    /// the same slot; if it is still in the window, the seed bytes are
    /// compared and the match extended. A copy of at least [`MIN_COPY`]
    /// bytes is emitted and matching resumes after it, with a fresh
    /// fingerprint. Otherwise the table slot is set to `p`, so the newest
    /// occurrence of a seed always wins (it is the one most likely to stay
    /// in the window).
    ///
    /// While matching, the ring holds only bytes *before* this frame; the
    /// frame itself is read from `frame`. A source byte at an absolute
    /// position below `start` is used only if the ring still retains it
    /// (`>= oldest`), so no source can have been overwritten. The frame is
    /// appended to the ring only after matching.
    pub fn frame(&mut self, frame: &[u8]) -> (Vec<u8>, Vec<u8>) {
        assert!(frame.len() <= self.hist.capacity() / 2, "window too small for frame");
        let start = self.hist.total;
        let oldest = self.hist.oldest();
        let hist = &self.hist;
        // Byte at absolute position `abs`, if available to a copy.
        let byte = |abs: u64| -> Option<u8> {
            if abs >= start {
                frame.get((abs - start) as usize).copied()
            } else if abs >= oldest {
                Some(hist.at(abs))
            } else {
                None
            }
        };
        let n = frame.len();
        let mut residual = Vec::with_capacity(n);
        // Each copy adds at most three varints and covers at least MIN_COPY
        // bytes; one more varint ends the list. The list therefore never
        // outgrows this capacity, which callers rely on for memory admission.
        let mut cmds = Vec::with_capacity(3 * MAX_VARINT_LEN * (n / MIN_COPY) + MAX_VARINT_LEN);
        let cmds_cap = cmds.capacity();
        let mut lit_start = 0usize;
        let mut p = 0usize;
        let window = self.hist.capacity() as u64;
        let mut h = if n >= SEED { Self::fingerprint(frame) } else { 0 };
        while p + SEED <= n {
            if Self::is_anchor(h) {
                let slot = Self::slot(h);
                let q = self.table[slot];
                let pa = start + p as u64;
                if q < pa && pa - q <= window && q >= oldest {
                    if let Some((cstart, len)) = extend(&byte, frame, p, q, lit_start) {
                        residual.extend_from_slice(&frame[lit_start..cstart]);
                        put_varint((cstart - lit_start) as u64, &mut cmds);
                        put_varint(len as u64, &mut cmds);
                        put_varint(pa - q, &mut cmds);
                        lit_start = cstart + len;
                        p = lit_start;
                        if p + SEED <= n {
                            h = Self::fingerprint(&frame[p..]);
                        }
                        continue;
                    }
                }
                self.table[slot] = pa;
            }
            if p + SEED < n {
                h = self.roll(h, frame[p], frame[p + SEED]);
            }
            p += 1;
        }
        residual.extend_from_slice(&frame[lit_start..]);
        put_varint((n - lit_start) as u64, &mut cmds);
        debug_assert_eq!(cmds.capacity(), cmds_cap, "command list outgrew its bound");
        for &b in frame {
            self.hist.push(b);
        }
        (residual, cmds)
    }
}

/// The copy of at least [`MIN_COPY`] bytes through the seed at `frame[p..]`
/// and absolute source position `q`, if the seed matches: extended
/// forward within the frame and backward, but not before the pending
/// literal run (`lit_start`) nor before what `byte` can read. Returns its
/// start in the frame and its length.
fn extend(
    byte: &impl Fn(u64) -> Option<u8>,
    frame: &[u8],
    p: usize,
    q: u64,
    lit_start: usize,
) -> Option<(usize, usize)> {
    if !(0..SEED).all(|i| byte(q + i as u64) == Some(frame[p + i])) {
        return None;
    }
    let mut f = SEED;
    while p + f < frame.len() && byte(q + f as u64) == Some(frame[p + f]) {
        f += 1;
    }
    let mut b = 0usize;
    while p - b > lit_start && q > b as u64 && byte(q - b as u64 - 1) == Some(frame[p - b - 1]) {
        b += 1;
    }
    let len = f + b;
    (len >= MIN_COPY).then_some((p - b, len))
}

/// The command list as stored: a selector byte, then either the raw varints
/// (`CMDS_RAW`) or the structured code below (`CMDS_CODED`), whichever is
/// smaller. Raw varints spend whole bytes on small numbers and repeat the
/// same distances verbatim; the structured code spends a few bits on each.
const CMDS_RAW: u8 = 0;
const CMDS_CODED: u8 = 1;

/// The most bits `x + 1` can have for a coded number `x`. Every number is
/// below the largest window, 2^36 (see `crate::format::MAX_DEDUP_LOG2`).
const MAX_FIELD_BITS: usize = 37;
/// Bits below the leading one that have adaptive models.
const MODELED: u32 = 4;
/// Distances kept on the recency list.
const RECENT: usize = 4;

/// An adaptive code for one kind of number: a literal run, a copy length or
/// a distance.
///
/// The number is written in Elias's gamma code (1975): for `v = x + 1` of `n`
/// bits, `n − 1` in unary and then the `n − 1` bits of `v` below its leading
/// one. Every bit goes through the binary coder. A unary bit has a model for
/// its position, so the coder learns how long the numbers are. The first
/// [`MODELED`] bits below the leading one have models chosen by `n` and the
/// bits before them, because the leading digits of a quantity are far from
/// uniform. The lower bits are close to uniform and are coded at one half.
///
/// P. Elias, *Universal Codeword Sets and Representations of the Integers*,
/// IEEE Trans. Inf. Theory 21(2), 1975, pp. 194–203.
struct Field {
    unary: [BitModel; MAX_FIELD_BITS],
    top: Vec<BitModel>,
}

impl Field {
    fn new() -> Self {
        Field { unary: [BitModel::new(); MAX_FIELD_BITS], top: vec![BitModel::new(); (MAX_FIELD_BITS + 1) << MODELED] }
    }

    fn put(&mut self, e: &mut RangeEncoder<Bounded>, x: u64) -> io::Result<()> {
        let v = x + 1;
        let n = (64 - v.leading_zeros()) as usize;
        debug_assert!(n <= MAX_FIELD_BITS);
        // The longest length needs no closing 0.
        for i in 1..=n.min(MAX_FIELD_BITS - 1) {
            e.encode_bit(&mut self.unary[i - 1], i < n)?;
        }
        let rest = n as u32 - 1;
        let top = rest.min(MODELED);
        let mut ctx = 1;
        for i in (rest - top..rest).rev() {
            let bit = (v >> i) & 1 != 0;
            e.encode_bit(&mut self.top[(n << MODELED) + ctx], bit)?;
            ctx = (ctx << 1) | usize::from(bit);
        }
        let flat = rest - top;
        e.encode_direct((v & ((1 << flat) - 1)) as u32, flat)
    }

    fn get(&mut self, d: &mut RangeDecoder) -> u64 {
        let mut n = 1;
        while n < MAX_FIELD_BITS && d.decode_bit(&mut self.unary[n - 1]) {
            n += 1;
        }
        let rest = n as u32 - 1;
        let top = rest.min(MODELED);
        let (mut v, mut ctx) = (1u64, 1);
        for _ in 0..top {
            let bit = d.decode_bit(&mut self.top[(n << MODELED) + ctx]);
            ctx = (ctx << 1) | usize::from(bit);
            v = (v << 1) | u64::from(bit);
        }
        let flat = rest - top;
        ((v << flat) | u64::from(d.decode_direct(flat))) - 1
    }
}

/// The models for the structured command code.
///
/// Records and tables repeat at a few distances, so the last [`RECENT`]
/// distinct distances are kept most recent first, as in the move-to-front
/// scheme of Bentley, Sleator, Tarjan and Wei (1986). A distance on the list
/// is coded as its rank, in unary, and moves to the front. Any other is coded
/// as a number and pushes the oldest off the list.
///
/// J. L. Bentley, D. D. Sleator, R. E. Tarjan and V. K. Wei, *A Locally
/// Adaptive Data Compression Scheme*, Comm. ACM 29(4), 1986, pp. 320–330.
struct CommandModel {
    count: Field,
    literal: Field,
    length: Field,
    distance: Field,
    /// Whether a distance is on the list, by whether the last one was.
    listed: [BitModel; 2],
    /// The unary bits of a rank.
    rank: [BitModel; RECENT - 1],
    recent: [u64; RECENT],
    last_listed: usize,
}

impl CommandModel {
    fn new() -> Self {
        CommandModel {
            count: Field::new(),
            literal: Field::new(),
            length: Field::new(),
            distance: Field::new(),
            listed: [BitModel::new(); 2],
            rank: [BitModel::new(); RECENT - 1],
            recent: [0; RECENT],
            last_listed: 0,
        }
    }

    /// Makes `i` the most recent distance.
    fn touch(&mut self, i: usize) {
        self.recent[..=i].rotate_right(1);
    }
}

/// Stores a frame's raw command list (as [`Deduper::frame`] makes it):
/// a selector byte and the smaller of the raw list and its structured
/// code. The structured code is the number of copies; for each copy its
/// literal run, its distance as a rank on the recency list or else as the
/// distance less one, and its length less
/// [`MIN_COPY`]; then the final literal run.
pub(crate) fn pack_commands(raw: &[u8]) -> Vec<u8> {
    let (selector, body) = match code_commands(raw) {
        Some(code) => (CMDS_CODED, code),
        None => (CMDS_RAW, raw.to_vec()),
    };
    let mut p = Vec::with_capacity(body.len() + 1);
    p.push(selector);
    p.extend_from_slice(&body);
    p
}

/// The structured code of `raw`, or `None` if it would not be smaller. Two
/// passes over the raw list (the first counts the copies) keep it free of
/// any intermediate list, and the code is written into a buffer that never
/// asks for more than the raw list's size.
fn code_commands(raw: &[u8]) -> Option<Vec<u8>> {
    let mut copies = 0u64;
    let mut pos = 0;
    loop {
        get_varint(raw, &mut pos).ok()?;
        if pos == raw.len() {
            break;
        }
        get_varint(raw, &mut pos).ok()?;
        get_varint(raw, &mut pos).ok()?;
        copies += 1;
    }
    let mut m = CommandModel::new();
    let mut e = RangeEncoder::new(Bounded::new(raw.len().checked_sub(1)?));
    m.count.put(&mut e, copies).ok()?;
    let mut pos = 0;
    for _ in 0..copies {
        let lit = get_varint(raw, &mut pos).ok()?;
        let len = get_varint(raw, &mut pos).ok()?;
        let dist = get_varint(raw, &mut pos).ok()?;
        m.literal.put(&mut e, lit).ok()?;
        let hit = m.recent.iter().position(|&r| r == dist);
        e.encode_bit(&mut m.listed[m.last_listed], hit.is_some()).ok()?;
        match hit {
            Some(i) => {
                // The last rank needs no closing 0.
                for k in 0..(i + 1).min(RECENT - 1) {
                    e.encode_bit(&mut m.rank[k], k < i).ok()?;
                }
                m.touch(i);
            }
            None => {
                m.distance.put(&mut e, dist - 1).ok()?;
                m.touch(RECENT - 1);
                m.recent[0] = dist;
            }
        }
        m.last_listed = usize::from(hit.is_some());
        m.length.put(&mut e, len - MIN_COPY as u64).ok()?;
    }
    m.literal.put(&mut e, get_varint(raw, &mut pos).ok()?).ok()?;
    e.finish().ok().map(Bounded::into_inner)
}

/// The raw command list of a stored one, for a frame of `raw_len` bytes.
/// Every decoded number is bounded before use; the list it returns is
/// checked again, in full, by [`Reconstructor::frame`].
pub(crate) fn unpack_commands(packed: &[u8], raw_len: usize) -> io::Result<Vec<u8>> {
    let (&selector, body) = packed.split_first().ok_or_else(|| corrupt("empty command list"))?;
    match selector {
        CMDS_RAW => return Ok(body.to_vec()),
        CMDS_CODED => {}
        _ => return Err(corrupt("unknown command encoding")),
    }
    let mut d = RangeDecoder::new(body)?;
    let mut m = CommandModel::new();
    // Every copy covers at least MIN_COPY bytes of the frame.
    let copies = m.count.get(&mut d);
    if copies > (raw_len / MIN_COPY) as u64 {
        return Err(corrupt("too many coded commands"));
    }
    let mut raw = Vec::with_capacity((3 * copies as usize + 1) * MAX_VARINT_LEN);
    for _ in 0..copies {
        put_varint(m.literal.get(&mut d), &mut raw);
        let dist = if d.decode_bit(&mut m.listed[m.last_listed]) {
            let mut i = 0;
            while i < RECENT - 1 && d.decode_bit(&mut m.rank[i]) {
                i += 1;
            }
            // An unset slot is 0, which the reconstructor rejects.
            let dist = m.recent[i];
            m.touch(i);
            m.last_listed = 1;
            dist
        } else {
            let dist = m.distance.get(&mut d) + 1;
            m.touch(RECENT - 1);
            m.recent[0] = dist;
            m.last_listed = 0;
            dist
        };
        let len = m.length.get(&mut d) + MIN_COPY as u64;
        put_varint(len, &mut raw);
        put_varint(dist, &mut raw);
    }
    put_varint(m.literal.get(&mut d), &mut raw);
    d.finish().map_err(|e| corrupt(&e.to_string()))?;
    Ok(raw)
}

/// Decoder side: rebuilds frames from residual + commands against the
/// history of output already produced.
pub struct Reconstructor {
    hist: History,
}

impl Reconstructor {
    /// `window` must equal the encoder's.
    pub fn new(window: usize) -> Self {
        Reconstructor { hist: History::new(window) }
    }

    /// Rebuilds a frame of `raw_len` bytes. Every length and distance
    /// is checked: copies reach back at most `min(window, produced)` bytes,
    /// never forward, and the frame must consume the whole residual and
    /// command list.
    pub fn frame(&mut self, residual: &[u8], cmds: &[u8], raw_len: usize) -> io::Result<Vec<u8>> {
        if raw_len > self.hist.capacity() / 2 {
            return Err(corrupt("frame larger than window allows"));
        }
        let mut out = Vec::with_capacity(raw_len);
        let (mut r, mut c) = (0usize, 0usize);
        loop {
            let lit = get_varint(cmds, &mut c)?;
            let lit = usize::try_from(lit).map_err(|_| corrupt("literal run too long"))?;
            if lit > residual.len() - r || lit > raw_len - out.len() {
                return Err(corrupt("literal run past end"));
            }
            for &b in &residual[r..r + lit] {
                out.push(b);
                self.hist.push(b);
            }
            r += lit;
            if c == cmds.len() {
                break;
            }
            let len = get_varint(cmds, &mut c)?;
            let dist = get_varint(cmds, &mut c)?;
            let len = usize::try_from(len).map_err(|_| corrupt("copy too long"))?;
            if len == 0 || len > raw_len - out.len() {
                return Err(corrupt("copy past end of frame"));
            }
            let reach = (self.hist.capacity() as u64).min(self.hist.total);
            if dist == 0 || dist > reach {
                return Err(corrupt("copy distance out of range"));
            }
            // Byte by byte: a copy may overlap its own output.
            let src = self.hist.total - dist;
            for i in 0..len as u64 {
                let b = self.hist.at(src + i);
                out.push(b);
                self.hist.push(b);
            }
        }
        if out.len() != raw_len || r != residual.len() {
            return Err(corrupt("frame length mismatch"));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            self.0 >> 24
        }
        fn bytes(&mut self, n: usize) -> Vec<u8> {
            (0..n).map(|_| self.next() as u8).collect()
        }
    }

    fn roundtrip(data: &[u8], frame: usize, window: usize) -> usize {
        let mut d = Deduper::new(window);
        let mut r = Reconstructor::new(window);
        let mut removed = 0;
        for chunk in data.chunks(frame) {
            let (res, cmds) = d.frame(chunk);
            removed += chunk.len() - res.len();
            assert_eq!(r.frame(&res, &cmds, chunk.len()).unwrap(), chunk);
        }
        removed
    }

    #[test]
    fn duplicated_blocks_far_apart_are_found() {
        let mut g = Rng(1);
        let block = g.bytes(4096);
        let mut data = block.clone();
        data.extend(g.bytes(3 << 20)); // 3 MiB of noise between copies
        data.extend_from_slice(&block);
        let removed = roundtrip(&data, 1 << 20, 8 << 20);
        assert!(removed >= 4096 - 2 * (1 << ANCHOR_BITS) - SEED, "removed {removed}");
    }

    #[test]
    fn beyond_window_is_not_copied() {
        let mut g = Rng(2);
        let block = g.bytes(8192);
        let mut data = block.clone();
        data.extend(g.bytes(5 << 20));
        data.extend_from_slice(&block);
        // Window 4 MiB < distance: nothing removed, still round-trips.
        assert_eq!(roundtrip(&data, 1 << 20, 4 << 20), 0);
    }

    #[test]
    fn changed_blocks_no_repeats_periodic_and_wrap() {
        let mut g = Rng(3);
        // Changed version of a block: one byte differs every 1000 bytes.
        let a = g.bytes(100_000);
        let mut b = a.clone();
        for i in (0..b.len()).step_by(1000) {
            b[i] ^= 0x55;
        }
        let mut data = a.clone();
        data.extend_from_slice(&b);
        assert!(roundtrip(&data, 1 << 16, 1 << 18) > 50_000);
        // No repeats.
        assert_eq!(roundtrip(&g.bytes(1 << 20), 1 << 16, 1 << 18), 0);
        // Periodic with a 1000-byte random period: copies overlap their own
        // output (distance < length).
        let unit = g.bytes(1000);
        let periodic: Vec<u8> = unit.iter().copied().cycle().take(300_000).collect();
        assert!(roundtrip(&periodic, 1 << 16, 1 << 18) > 250_000);
        // Very short periods may contain no anchor at all; they must still
        // round-trip (the block model handles them well).
        let short: Vec<u8> = (0..300_000).map(|i| (i % 7) as u8).collect();
        roundtrip(&short, 1 << 16, 1 << 18);
        // Ring wrap: far more data than the window, with repeats throughout.
        let unit = g.bytes(5000);
        let wrap: Vec<u8> = (0..400).flat_map(|i| if i % 3 == 0 { g.bytes(5000) } else { unit.clone() }).collect();
        assert!(roundtrip(&wrap, 1 << 15, 1 << 16) > 1_000_000);
    }

    /// A source near the window edge must survive while the current frame
    /// is matched: with W = 8192 and 4096-byte frames, part of frame A
    /// repeats inside frame C, two frames later.
    #[test]
    fn sources_at_window_edge_are_intact() {
        let mut g = Rng(42);
        let a = g.bytes(4096);
        let b = g.bytes(4096);
        let mut c = g.bytes(4096);
        c[1500..1800].copy_from_slice(&a[1500..1800]);
        let mut data = a.clone();
        data.extend_from_slice(&b);
        data.extend_from_slice(&c);
        roundtrip(&data, 4096, 8192);
        // And a sweep of partial repeats at every offset band and window.
        for w in [1usize << 13, 1 << 14] {
            for off in (0..3800).step_by(300) {
                let mut g = Rng(off as u64 + w as u64);
                let a = g.bytes(4096);
                let mut frames = a.clone();
                for _ in 0..4 {
                    let mut f = g.bytes(4096);
                    f[off..off + 300].copy_from_slice(&a[off..off + 300]);
                    frames.extend_from_slice(&f);
                }
                roundtrip(&frames, 4096, w);
            }
        }
    }

    /// Command lists from real frames survive packing, whichever encoding
    /// the packer picks; damaged packed lists are rejected or decode to
    /// something the reconstructor then checks, and never panic.
    #[test]
    fn command_lists_pack_and_unpack() {
        let mut g = Rng(9);
        let unit = g.bytes(3000);
        let mut data = Vec::new();
        for i in 0..300 {
            if i % 4 == 0 {
                data.extend(g.bytes(700));
            } else {
                data.extend_from_slice(&unit[(i * 37) % 1000..]);
            }
        }
        let mut d = Deduper::new(1 << 22);
        let mut coded = 0;
        for chunk in data.chunks(1 << 18) {
            let (_res, raw) = d.frame(chunk);
            let packed = pack_commands(&raw);
            assert!(packed.len() <= raw.len() + 1);
            coded += usize::from(packed[0] == CMDS_CODED);
            assert_eq!(unpack_commands(&packed, chunk.len()).unwrap(), raw);
            let mut s = 7u64;
            for _ in 0..200 {
                let mut bad = packed.clone();
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let i = 1 + (s >> 33) as usize % (bad.len() - 1).max(1);
                if i < bad.len() {
                    bad[i] ^= 1 << ((s >> 29) & 7);
                }
                let cut = if s % 3 == 0 { bad.len() / 2 } else { bad.len() };
                let _ = unpack_commands(&bad[..cut.max(1)], chunk.len());
            }
        }
        assert!(coded > 0, "no list chose the coded form");
        // A list with no copies, an unknown selector, and too many copies.
        let mut none = Vec::new();
        put_varint(5, &mut none);
        assert_eq!(unpack_commands(&pack_commands(&none), 5).unwrap(), none);
        assert!(unpack_commands(&[2, 0], 5).is_err());
        assert!(unpack_commands(&[], 5).is_err());
        let mut many = Vec::new();
        for _ in 0..3 {
            put_varint(0, &mut many);
            put_varint(MIN_COPY as u64, &mut many);
            put_varint(1, &mut many);
        }
        put_varint(0, &mut many);
        let code = code_commands(&many).unwrap();
        let packed = [&[CMDS_CODED][..], &code].concat();
        assert_eq!(unpack_commands(&packed, 3 * MIN_COPY).unwrap(), many);
        assert!(unpack_commands(&packed, 3 * MIN_COPY - 1).is_err(), "copies must fit the frame");
        // A field codes every number below 2^36, and the longest length is
        // the one without a closing bit.
        let mut values = vec![0, 1, 2, 255, 256, (1 << 36) - 2, (1 << 36) - 1];
        values.extend((1..36).flat_map(|k| [(1u64 << k) - 1, 1 << k, (1 << k) + 1]));
        let (mut f, mut e) = (Field::new(), RangeEncoder::new(Bounded::new(1 << 12)));
        for &x in &values {
            f.put(&mut e, x).unwrap();
        }
        let code = e.finish().unwrap().into_inner();
        let (mut f, mut d) = (Field::new(), RangeDecoder::new(&code).unwrap());
        for &x in &values {
            assert_eq!(f.get(&mut d), x);
        }
        d.finish().unwrap();
    }

    #[test]
    fn varints_are_canonical() {
        let mut pos = 0;
        assert_eq!(get_varint(&[0x80, 0x01], &mut pos).unwrap(), 128);
        let mut v = Vec::new();
        put_varint(u64::MAX, &mut v);
        let mut pos = 0;
        assert_eq!(get_varint(&v, &mut pos).unwrap(), u64::MAX);
        // Tenth byte carrying more than one bit overflows.
        let mut bad = v.clone();
        *bad.last_mut().unwrap() = 0x02;
        assert!(get_varint(&bad, &mut 0).is_err());
        // Overlong: 0 encoded in two bytes.
        assert!(get_varint(&[0x80, 0x00], &mut 0).is_err());
        assert!(get_varint(&[0x80], &mut 0).is_err());
        assert!(get_varint(&[0xFF; 11], &mut 0).is_err());
    }

    #[test]
    fn hostile_commands_are_rejected() {
        let mut r = Reconstructor::new(1 << 16);
        let mut cmds = Vec::new();
        // literal 1, copy 10 at distance 5 (only 1 byte produced).
        put_varint(1, &mut cmds);
        put_varint(10, &mut cmds);
        put_varint(5, &mut cmds);
        put_varint(0, &mut cmds);
        assert!(r.frame(b"a", &cmds, 11).is_err());
        let mut r = Reconstructor::new(1 << 16);
        // Distance zero.
        let mut cmds = Vec::new();
        put_varint(1, &mut cmds);
        put_varint(3, &mut cmds);
        put_varint(0, &mut cmds);
        put_varint(0, &mut cmds);
        assert!(r.frame(b"a", &cmds, 4).is_err());
        // Copy past declared frame length; truncated list; leftover residual.
        let mut r = Reconstructor::new(1 << 16);
        let mut cmds = Vec::new();
        put_varint(2, &mut cmds);
        put_varint(9, &mut cmds);
        put_varint(1, &mut cmds);
        put_varint(0, &mut cmds);
        assert!(r.frame(b"ab", &cmds, 5).is_err());
        assert!(Reconstructor::new(1 << 16).frame(b"ab", &[0x80], 2).is_err());
        let mut cmds = Vec::new();
        put_varint(1, &mut cmds);
        assert!(Reconstructor::new(1 << 16).frame(b"ab", &cmds, 2).is_err());
        // A frame larger than half the window.
        assert!(Reconstructor::new(1 << 4).frame(&[0; 9], &[9], 9).is_err());
    }
}
