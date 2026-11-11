// Copyright (c) 2026 Darrell D. E. Long. All rights reserved.
// Use and redistribution are subject to the BSD 3-clause license, with a
// dedication clause, in the LICENSE file at the root of this repository.
//
// Dedicated to the memory of my colleague Glen G. Langdon

//! Prediction by partial matching (Cleary and Witten, 1984) with contexts of up
//! to [`MAX_ORDER`] bytes, kept in a trie with suffix links (Moffat, 1990) and
//! estimated as in Shkarin's PPMII (2002).
//!
//! A byte is coded in the longest context that has seen it. A context that has
//! not seen it codes an escape, its symbols are excluded from the shorter
//! contexts that follow, and coding moves to the next shorter context. Below
//! order 0 every byte not excluded is equally likely. The byte is then counted
//! in the context that coded it and added to every longer one (update
//! exclusion). The events go to the range coder of Moffat, Neal and Witten
//! (1998), [`arith::MultiRangeEncoder`].
//!
//! # Estimates
//!
//! Section numbers are Shkarin's. Where the paper leaves a parameter to its
//! reference \[7\], which we have not used, the choice is ours and is described
//! at the function named.
//!
//! * A symbol seen `c` times has weight `2c − 1` (Howard and Vitter's method
//!   D). Counts are kept in half units, which makes a half count exact.
//!
//! * A context of one symbol codes a binary decision, the symbol or an escape,
//!   with a probability learned for its class (§3.1; `Ppm::binary_class`).
//!
//! * A larger context gives the escape a frequency learned for its class, coded
//!   against the weights of its visible symbols (§3.3 and §5.5;
//!   `Ppm::escape_class`, `Ppm::learn_escape`).
//!
//! * A symbol found below the longest order is also counted, at half weight, in
//!   the next shorter context while its count is under 8 (§2.2;
//!   `updates_parent`).
//!
//! * A symbol added to a context starts with a count inherited from the context
//!   that coded it (§2.1; `inherited_count`). A new context gets its count when
//!   it is next used (`Ppm::resolve_pending`).
//!
//! # Storage
//!
//! The contexts form a trie held in two arrays and linked by index (Knuth, vol.
//! 3, §6.3). A node is a context. Its symbols are its children, kept together
//! in an array of slots; a context of one symbol, the commonest kind at the
//! longer orders, keeps it in the node. Symbol `s` of context `c` points to the
//! node of `c·s`, the context that follows when `s` is coded in `c`. At the
//! longest order, where `c·s` would be too long, it points to `c·s` without its
//! oldest byte. Every node also points to its suffix, the context without its
//! oldest byte (Moffat's vine pointer). Once a byte is coded, the symbol it was
//! found as names the next byte's longest context, and an escape follows the
//! suffix pointer.
//!
//! This suffices because update exclusion changes the counts but not which
//! symbols a context holds. The first time `s` followed a context, every longer
//! context escaped, so all of them received `s` with it. Hence `c` holds `s`
//! exactly when `c·s` has occurred, and every node is reached from the symbol
//! of its last byte. The first contexts after a restart are the exception. They
//! are created from the history without the contexts that lead to them, and
//! wait on a short list until the symbol that leads to each is added (see
//! `Ppm::update`).
//!
//! When the nodes or the slots run out, every context and count is discarded.
//! The preceding bytes and the learned classes are kept, and the byte being
//! learned is counted as if new.
//!
//! Both arrays are reserved once, at the most a block can need, and never grow,
//! so nodes and arrays never move. A restart empties them but keeps their
//! storage.
//!
//! # References
//!
//! J. G. Cleary and I. H. Witten, *Data Compression Using Adaptive Coding and
//! Partial String Matching*, IEEE Trans. Commun. 32(4), 1984, pp. 396–402.
//!
//! A. Moffat, *Implementing the PPM Data Compression Scheme*, IEEE Trans.
//! Commun. 38(11), 1990, pp. 1917–1921.
//!
//! D. E. Knuth, *The Art of Computer Programming*, vol. 3, *Sorting and
//! Searching*, 2nd ed., Addison-Wesley, 1998, §6.3 (tries); vol. 1,
//! *Fundamental Algorithms*, 3rd ed., 1997, §2.5 (free lists by size).
//!
//! P. G. Howard and J. S. Vitter, *Practical Implementations of Arithmetic
//! Coding*, in J. A. Storer (ed.), *Image and Text Compression*, Kluwer, 1992,
//! §3.5 (method D).
//!
//! C. Bloom, *Solving the Problems of Context Modeling*, 1998 (secondary escape
//! estimation).
//!
//! D. Shkarin, *PPM: One Step to Practicality*, Proc. Data Compression
//! Conference, 2002, pp. 202–211.
//!
//! H. Robbins and S. Monro, *A Stochastic Approximation Method*, Ann. Math.
//! Statist. 22(3), 1951, pp. 400–407.
//!
//! A. Moffat, R. M. Neal and I. H. Witten, *Arithmetic Coding Revisited*, ACM
//! Trans. Inf. Syst. 16(3), 1998, pp. 256–294.

use std::io::{self, Write};

use arith::{MultiRangeDecoder, MultiRangeEncoder};

/// Longest context, in bytes.
pub const MAX_ORDER: usize = 6;
/// A context's counts are halved when its total passes this.
const MAX_TOTAL: u32 = 1 << 12;
/// Symbol slots before a restart.
const SYM_SLOTS: usize = 1 << 24;
/// Contexts before a restart. The format fixes this value.
const MAX_CONTEXTS: usize = 6_291_454;
// A context grows its symbol array by doubling, up to 256 entries, so the
// arena must hold one full array per order.
const _: () = assert!(SYM_SLOTS >= 256 * MAX_ORDER);

/// One symbol of a context of two or more: 8 bytes.
#[derive(Clone, Copy, Default)]
struct Slot {
    /// The context that follows when this symbol is coded (see the module
    /// documentation); 0 until that context exists.
    next: u32,
    count: u16,
    byte: u8,
}

/// A context: 12 bytes. A context of one symbol, the commonest kind at the
/// longer orders, keeps it here; its count is the total.
#[derive(Clone, Copy, Default)]
struct Node {
    /// The context without its oldest byte; 0 (no node) for order 1.
    suffix: u32,
    /// With one symbol, the context it leads to (its `next`); with more,
    /// the first slot of their array of [`capacity`]`(len)` slots.
    link: u32,
    /// Sum of the counts of its symbols (at most MAX_TOTAL plus one
    /// update's worth before it is halved); 0 while the first count of a
    /// new context is pending (see `Ppm::resolve_pending`).
    total: u16,
    /// Symbols less one.
    more: u8,
    /// With one symbol, that symbol.
    byte: u8,
}
const _: () = assert!(2 * MAX_TOTAL + 64 <= u16::MAX as u32);

/// Halves a count in half units, rounding up to a whole count, so no count
/// reaches zero.
fn halve(h: u16) -> u16 {
    h.div_ceil(4) * 2
}

impl Node {
    fn len(&self) -> usize {
        usize::from(self.more) + 1
    }
}

/// Where a context keeps one of its symbols.
#[derive(Clone, Copy, PartialEq, Debug)]
enum At {
    /// In node `i`, its only symbol.
    Node(u32),
    /// In this slot of the arena.
    Slot(u32),
}

/// The symbol arrays of the contexts of two or more symbols, each of a
/// power-of-two size, in one array of slots. An array a context outgrows
/// goes on the free list of its size and is handed out again before the
/// arena grows (Knuth, *TAOCP* vol. 1, §2.5); the list is linked through
/// the first slot of each free array.
struct Arena {
    slots: Vec<Slot>,
    /// The first free array of each size `2^k`, or [`NO_ARRAY`].
    free: [u32; 9],
}

const NO_ARRAY: u32 = u32::MAX;

impl Arena {
    /// Returns an arena with room for `n` slots, reserved but not yet touched.
    fn new(n: usize) -> Self {
        Arena { slots: Vec::with_capacity(n), free: [NO_ARRAY; 9] }
    }

    /// Returns the number of slots taken from the end of the arena.
    fn allocated(&self) -> usize {
        self.slots.len()
    }

    fn clear(&mut self) {
        self.slots.clear();
        self.free = [NO_ARRAY; 9];
    }

    /// Returns the first slot of an array of `n` slots. `n` is a power of two,
    /// at most 256.
    fn alloc(&mut self, n: usize) -> usize {
        let k = n.trailing_zeros() as usize;
        let head = self.free[k];
        if head != NO_ARRAY {
            self.free[k] = self.slots[head as usize].next;
            return head as usize;
        }
        let start = self.slots.len();
        self.slots.resize(start + n, Slot::default());
        start
    }

    /// Returns the array of `n` slots at `start` to its free list.
    fn release(&mut self, start: usize, n: usize) {
        let k = n.trailing_zeros() as usize;
        self.slots[start].next = self.free[k];
        self.free[k] = start as u32;
    }
}

/// Returns the slots in the symbol array of a context with `len` symbols, at
/// least two: the next power of two, up to all 256 byte values.
fn capacity(len: usize) -> usize {
    len.next_power_of_two().min(256)
}

/// Returns the packed bytes of a context, for matching the contexts created
/// right after a restart: the low `order` bytes of `bytes`, and the order.
fn key(bytes: u64, order: usize) -> u64 {
    let mask = (1u64 << (8 * order)) - 1;
    ((bytes & mask) << 3) | order as u64
}

/// Where a byte was found while coding it.
#[derive(Clone, Copy)]
enum Found {
    /// In the context of this order, there, with the byte's weight and the
    /// total it was coded against.
    Ctx { order: usize, at: At, w: u32, t: u32 },
    /// In the order-0 table.
    Order0 { w: u32, t: u32 },
    /// Nowhere: coded below order 0.
    Nowhere,
}

/// Reports whether a symbol found in a context of `order`, where its count was
/// `count` half units before this byte, is also counted at half weight in the
/// next shorter context (§2.2). The paper updates the parent while the symbol's
/// frequency in the found context is under 8, and never from the longest order,
/// whose statistics it takes to be stable. We read that frequency as the stored
/// count.
fn updates_parent(order: usize, count: u16) -> bool {
    order < MAX_ORDER && count < 16
}

/// Returns the first count, in half units, of a byte added to a context that
/// holds `m_i` symbols with total `t_i`. The byte had count `h_a` in the
/// context that coded it, whose total is `t_k`.
///
/// This is equation (5) of §2.1 with τ = 1/4 and PPMD's frequencies: the byte's
/// frequency where it was coded is `t(a|s_k) = f − 1/2`, and `T(s)`, which
/// includes the escape frequency `m(s)/2`, is `Σ f`. Then
/// `t₀ = τ + T(s_i)·(t(a|s_k) − τ) / (T(s_k) − t(a|s_k) + T(s_i) − m(s_i))` and
/// the count is `t₀ + 1/2`, kept between 1 and 32. The arithmetic is in quarter
/// units, where τ is an integer. The denominator is at least 1/2 because every
/// count is at least 1.
fn inherited_count(h_a: u16, t_k: u16, t_i: u16, m_i: usize) -> u16 {
    let (h_a, t_k, t_i, m_i) = (u64::from(h_a), u64::from(t_k), u64::from(t_i), m_i as u64);
    let num = 2 * t_i * (2 * h_a - 3);
    let den = 2 * t_k - 2 * h_a + 2 + 2 * t_i - 4 * m_i;
    // 4·t₀, then f₀ = t₀ + 1/2 in half units: 2·t₀ + 1.
    let t0_4 = 1 + num / den;
    (t0_4 / 2 + 1).clamp(2, 64) as u16
}

/// Returns the first count, in half units, of a new context whose byte has
/// weight `w` of total `t` in the parent. Weights are `h − 1` and counts are in
/// half units, so `t(a|s_k) = w/2` and `T(s_k) = t/2`.
///
/// This is equation (5) of §2.1 for a context with `T(s_i) = 0`, with the
/// values §3.1 gives for binary contexts, τ = 1 and `t₀(esc|s_i) = 1`:
/// `t₀ = 1 + (t(a|s_k) − 1) / (T(s_k) − t(a|s_k))`. The count is `t₀` itself,
/// since the single-symbol classes read a count as PPMC does. It is rounded
/// down and kept between 1 and 32.
fn new_context_count(w: u32, t: u32) -> u16 {
    let (w, t) = (i64::from(w), i64::from(t));
    (2 + 2 * (w - 2) / (t - w)).clamp(2, 64) as u16
}

/// Classes of single-symbol contexts (§3.1).
const BIN_CLASSES: usize = 128 * 4 * 2 * 2 * 2 * 2;
/// The 16-bit scale of the coder's binary events.
const BIN_ONE: u32 = 1 << 16;
/// Bounds that keep a class's escape probability away from 0 and 1.
const BIN_MIN: u32 = 32;
/// A single-symbol class's estimate moves `1/2^BIN_SHIFT` of the way
/// toward each outcome.
const BIN_SHIFT: u32 = 6;

/// Escape classes of larger contexts: 400 with exclusions (§3.3), 200
/// without (§5.5).
const ESCAPE_CLASSES: usize = 400 + 200;
/// Fraction bits of an escape frequency.
const T_FRAC: u32 = 12;
/// The largest escape frequency, in weight units: above any context's total,
/// and far enough below the coder's limit that `T + t` always fits.
const T_MAX: u32 = 1 << 18;
const _: () = assert!(2 * MAX_TOTAL + 64 + T_MAX <= arith::MULTI_MAX_TOTAL);

/// Returns one of 128 levels for a count in half units: exact below 64, then
/// four octaves of eight steps each (the leading bit and the three below it).
fn count_level(h: u16) -> usize {
    if h < 64 {
        return usize::from(h);
    }
    let e = 15 - h.leading_zeros() as usize;
    (64 + 8 * (e - 6) + usize::from((h >> (e - 3)) & 7)).min(127)
}

/// The PPM model shared by the encoder and the decoder.
pub struct Ppm {
    /// Each single-symbol class's escape probability in 16 bits; 0 until
    /// the class is first used.
    bin_escape: Vec<u16>,
    /// Each escape class's escape frequency, with [`T_FRAC`] fraction bits;
    /// 0 until the class is first used.
    esc_freq: Vec<u32>,
    /// How often each escape class has been used (saturating), for its
    /// adaptation rate.
    esc_uses: Vec<u16>,
    /// Whether the last byte had a probability strictly above 1/2 in the event
    /// that coded it. A byte coded below order 0 counts as unlikely.
    prev_likely: bool,
    /// The number of bytes in a row each coded in the first context tried with
    /// a probability strictly above 1/2.
    run: u32,
    /// Whether the last byte escaped from the first context tried.
    escaped_last: bool,
    /// Bytes with a nonzero order-0 count.
    order0_len: u16,
    /// Node 0 is not a context; as an index it means "none".
    nodes: Vec<Node>,
    /// The model restarts before the contexts, with a byte's new ones,
    /// pass this.
    max_contexts: usize,
    slots: Arena,
    /// Symbol slots before a restart.
    sym_slots: usize,
    /// Restarts so far, for diagnostics.
    restarts: u32,
    /// Order 0: every byte's count, kept apart because it is dense.
    order0: [u16; 256],
    order0_total: u32,
    /// The order-1 context of each byte (0 if none).
    order1: [u32; 256],
    hist: u64,
    seen: usize,
    /// `excluded[b] == stamp` while byte `b` is excluded.
    excluded: [u32; 256],
    stamp: u32,
    any_excluded: bool,
    /// The longest context of the next byte, and its order (0: none).
    top: u32,
    top_order: usize,
    /// The context of each order for the current byte, from `top_order`
    /// down to where the byte was found, filled while coding it; the
    /// longer ones are filled while learning it.
    ctx: [u32; MAX_ORDER + 1],
    /// Where the last byte learned is kept in the contexts that learned it,
    /// by order. The contexts the next byte creates are linked from there.
    learned: [At; MAX_ORDER + 1],
    /// Whether `learned` refers to contexts that still exist.
    learned_valid: bool,
    /// Contexts created right after a restart that no symbol leads to yet,
    /// by key (see the module documentation).
    orphans: [(u64, u32); MAX_ORDER - 1],
    n_orphans: usize,
}

impl Ppm {
    /// Returns the most memory a model for a block of `block_size` bytes can
    /// come to hold: its two arrays as reserved, and the learned tables.
    /// Admission charges this.
    pub fn memory(block_size: usize) -> u64 {
        let (slots, nodes) = Self::capacities(SYM_SLOTS, MAX_CONTEXTS, block_size);
        let arrays = slots * std::mem::size_of::<Slot>() + nodes * std::mem::size_of::<Node>();
        let tables = BIN_CLASSES * std::mem::size_of::<u16>()
            + ESCAPE_CLASSES * (std::mem::size_of::<u32>() + std::mem::size_of::<u16>());
        (arrays + tables + std::mem::size_of::<Self>()) as u64
    }

    /// Returns the slots and nodes to reserve for a block of `len` bytes. A
    /// byte adds at most [`MAX_ORDER`] contexts and adds itself to at most that
    /// many. A context that comes to hold `n` symbols has used arrays of 2, 4,
    /// … up to fewer than `2n` slots, under `4n` in all, so the arena never
    /// passes `4 · MAX_ORDER · len` slots. Node 0 and one byte's new contexts
    /// come on top of the context limit.
    fn capacities(sym_slots: usize, max_contexts: usize, len: usize) -> (usize, usize) {
        // Saturating: blocks of up to 1 GiB would overflow a 32-bit usize.
        let slots = sym_slots.min(len.saturating_mul(4 * MAX_ORDER));
        let nodes = 1 + max_contexts.min(len.saturating_mul(MAX_ORDER)) + MAX_ORDER;
        (slots, nodes)
    }

    /// Returns a model for a block of `len` bytes.
    pub fn new(len: usize) -> Self {
        Self::with_limits(SYM_SLOTS, MAX_CONTEXTS, len)
    }

    /// Returns a model with the given limits for a block of `len` bytes. Small
    /// limits force frequent restarts, for testing.
    fn with_limits(sym_slots: usize, max_contexts: usize, len: usize) -> Self {
        // A restart must leave room for more than one byte's updates.
        assert!(sym_slots > 2 * 256 * MAX_ORDER && max_contexts > 2 * MAX_ORDER);
        let (slot_capacity, node_capacity) = Self::capacities(sym_slots, max_contexts, len);
        let mut m = Ppm {
            bin_escape: vec![0; BIN_CLASSES],
            esc_freq: vec![0; ESCAPE_CLASSES],
            esc_uses: vec![0; ESCAPE_CLASSES],
            prev_likely: false,
            run: 0,
            escaped_last: false,
            order0_len: 0,
            nodes: Vec::with_capacity(node_capacity),
            max_contexts,
            slots: Arena::new(slot_capacity),
            sym_slots,
            restarts: 0,
            order0: [0; 256],
            order0_total: 0,
            order1: [0; 256],
            hist: 0,
            seen: 0,
            excluded: [0; 256],
            stamp: 0,
            any_excluded: false,
            top: 0,
            top_order: 0,
            ctx: [0; MAX_ORDER + 1],
            learned: [At::Node(0); MAX_ORDER + 1],
            learned_valid: false,
            orphans: [(0, 0); MAX_ORDER - 1],
            n_orphans: 0,
        };
        m.nodes.push(Node::default());
        m
    }

    /// Forgets every context and count. The arrays keep their storage.
    fn restart(&mut self) {
        self.restarts += 1;
        self.nodes.truncate(1);
        self.slots.clear();
        self.order0 = [0; 256];
        self.order0_total = 0;
        self.order0_len = 0;
        self.order1 = [0; 256];
        self.top = 0;
        self.top_order = 0;
        self.learned_valid = false;
        self.n_orphans = 0;
    }

    /// Creates a context holding `byte`, its count pending, leading nowhere
    /// yet.
    fn new_node(&mut self, suffix: u32, byte: u8) -> u32 {
        self.nodes.push(Node { suffix, link: 0, total: 0, more: 0, byte });
        (self.nodes.len() - 1) as u32
    }

    /// Returns the symbols of a context of two or more.
    fn symbols(&self, c: &Node) -> &[Slot] {
        &self.slots.slots[c.link as usize..c.link as usize + c.len()]
    }

    fn next_of(&self, at: At) -> u32 {
        match at {
            At::Node(i) => self.nodes[i as usize].link,
            At::Slot(s) => self.slots.slots[s as usize].next,
        }
    }

    /// Returns the count, in half units, of the symbol at `at`.
    fn count_at(&self, at: At) -> u16 {
        match at {
            At::Node(i) => self.nodes[i as usize].total,
            At::Slot(s) => self.slots.slots[s as usize].count,
        }
    }

    fn byte_at(&self, at: At) -> u8 {
        match at {
            At::Node(i) => self.nodes[i as usize].byte,
            At::Slot(s) => self.slots.slots[s as usize].byte,
        }
    }

    fn set_next(&mut self, at: At, n: u32) {
        match at {
            At::Node(i) => self.nodes[i as usize].link = n,
            At::Slot(s) => self.slots.slots[s as usize].next = n,
        }
    }

    fn is_excluded(&self, b: u8) -> bool {
        self.excluded[usize::from(b)] == self.stamp
    }

    /// Returns all ones if `b` is not excluded and zero if it is. Masking a
    /// symbol's weight with it spares the scans a branch on the exclusion,
    /// which the processor cannot predict.
    fn seen_mask(&self, b: u8) -> u32 {
        u32::from(self.excluded[usize::from(b)] == self.stamp).wrapping_sub(1)
    }

    /// Prepares to code a byte: nothing is excluded and no context on the
    /// branch is pending. Byte `b` is excluded while `excluded[b]` equals the
    /// stamp, so a new stamp clears every exclusion at once. The array is reset
    /// only when the stamp wraps.
    fn begin_byte(&mut self) {
        self.any_excluded = false;
        self.stamp = self.stamp.wrapping_add(1);
        if self.stamp == 0 {
            self.excluded = [0; 256];
            self.stamp = 1;
        }
        self.resolve_pending();
    }

    /// Gives each pending context on the current branch its first count.
    ///
    /// A new context holds its byte with count 0, which marks it pending.
    /// Shkarin (§2.1, last paragraph) delays equation (5) until the context is
    /// next met, when the parent's statistics are closer to what the context
    /// will see. Nothing reads a pending count before then: coding, the parent
    /// update and both inheritances touch only contexts on the branch being
    /// coded, and a pending context gains no symbols. Resolving the branch's
    /// pending contexts before each byte, shortest first, is therefore enough.
    ///
    /// Each count comes from the parent, which holds the byte because every
    /// suffix of a context holds that context's symbols. `new_context_count`
    /// gives the estimate. A parent of one symbol passes on its own count
    /// (PPMC: `T − t = 1`).
    fn resolve_pending(&mut self) {
        let mut chain = [0u32; MAX_ORDER];
        let mut n = 0;
        let mut i = self.top;
        while i != 0 && self.nodes[i as usize].total == 0 {
            chain[n] = i;
            n += 1;
            i = self.nodes[i as usize].suffix;
        }
        for &p in chain[..n].iter().rev() {
            let (a, s) = (self.nodes[p as usize].byte, self.nodes[p as usize].suffix);
            let h = if s == 0 {
                match self.order0[usize::from(a)] {
                    0 => 2,
                    h_a => new_context_count(u32::from(h_a) - 1, self.order0_total),
                }
            } else {
                let c = self.nodes[s as usize];
                if c.more == 0 {
                    if c.byte == a {
                        c.total.clamp(2, 64)
                    } else {
                        2
                    }
                } else {
                    match self.symbols(&c).iter().find(|x| x.byte == a) {
                        Some(x) => new_context_count(u32::from(x.count) - 1, u32::from(c.total)),
                        None => 2,
                    }
                }
            };
            self.nodes[p as usize].total = h;
        }
    }

    fn exclude(&mut self, b: u8) {
        self.excluded[usize::from(b)] = self.stamp;
        self.any_excluded = true;
    }

    /// Excludes the symbols of a context of two or more.
    fn exclude_context(&mut self, c: &Node) {
        let stamp = self.stamp;
        for s in &self.slots.slots[c.link as usize..c.link as usize + c.len()] {
            self.excluded[usize::from(s.byte)] = stamp;
        }
        self.any_excluded = true;
    }

    fn exclude_order0(&mut self) {
        for b in 0..=255u8 {
            if self.order0[usize::from(b)] > 0 {
                self.exclude(b);
            }
        }
    }

    /// Returns the weight of a symbol with `count` half units: `2c − 1` for a
    /// symbol seen `c` times (method D).
    fn weight(count: u16) -> u32 {
        u32::from(count) - 1
    }

    /// Returns the sum of the weights of the symbols of `c` that are not
    /// excluded, and their number. `c` has two or more symbols.
    fn visible(&self, c: &Node) -> (u32, u32) {
        if !self.any_excluded {
            // Nothing excluded: Σ (2c − 1) over the symbols is 2N − d.
            let d = c.len() as u32;
            return (u32::from(c.total) - d, d);
        }
        let (mut sum, mut n) = (0, 0);
        for s in self.symbols(c) {
            let seen = self.seen_mask(s.byte);
            sum += Self::weight(s.count) & seen;
            n += seen & 1;
        }
        (sum, n)
    }

    fn order0_visible(&self) -> (u32, u32) {
        let (mut sum, mut n) = (0, 0);
        for b in 0..=255u8 {
            let c = self.order0[usize::from(b)];
            if c > 0 && !self.is_excluded(b) {
                sum += Self::weight(c);
                n += 1;
            }
        }
        (sum, n)
    }

    /// Returns the escape probability of a single-symbol context, in 16 bits,
    /// and its class.
    fn binary_estimate(&mut self, c: &Node) -> (u16, usize) {
        debug_assert!(c.total != 0, "a pending context's count was read");
        let k = self.binary_class(c);
        if self.bin_escape[k] == 0 {
            // First use: PPMC's escape, one in f + 1 for a symbol seen f times;
            // with the count h = 2f in half units, 2/(h + 2).
            let h = u32::from(c.total);
            self.bin_escape[k] = (2 * BIN_ONE / (h + 2)).clamp(BIN_MIN, BIN_ONE - BIN_MIN) as u16;
        }
        (self.bin_escape[k], k)
    }

    /// Returns the class of a single-symbol context (§3.1). The paper leaves
    /// the classes to its reference \[7\]. Ours combine:
    ///
    /// * the symbol's count in 128 levels (`count_level`);
    ///
    /// * the number of symbols in the next shorter context, as 1, 2–3, 4–15 or
    ///   more; for an order-1 context, the bytes in the order-0 table;
    ///
    /// * whether the previous byte was likely (`prev_likely`);
    ///
    /// * whether the previous byte has either of its top two bits set, and
    ///   whether the context's symbol does;
    ///
    /// * whether a run of more than [`MAX_ORDER`] likely bytes is under way
    ///   (`run`).
    fn binary_class(&self, c: &Node) -> usize {
        let shorter = if c.suffix == 0 { self.order0_len } else { self.nodes[c.suffix as usize].len() as u16 };
        let shorter = match shorter {
            0..=1 => 0,
            2..=3 => 1,
            4..=15 => 2,
            _ => 3,
        };
        let prev = (self.hist & 0xFF) as u8;
        let flags = usize::from(self.prev_likely) << 3
            | usize::from(prev >= 0x40) << 2
            | usize::from(self.run > MAX_ORDER as u32) << 1
            | usize::from(c.byte >= 0x40);
        (count_level(c.total) * 4 + shorter) * 16 + flags
    }

    /// Moves the escape probability of class `k` `1/2^BIN_SHIFT` of the way
    /// toward the outcome, within [`BIN_MIN`] of either end.
    fn learn_binary(&mut self, k: usize, escape: bool) {
        let p = i64::from(self.bin_escape[k]);
        let target = if escape { i64::from(BIN_ONE) } else { 0 };
        let p = p + ((target - p) >> BIN_SHIFT);
        self.bin_escape[k] = p.clamp(i64::from(BIN_MIN), i64::from(BIN_ONE - BIN_MIN)) as u16;
    }

    /// Returns the weight of the escape in context `c`, which has `visible`
    /// symbols not excluded and was reached by escaping from a context of
    /// `child` symbols, and the escape's class.
    ///
    /// One context's counts say little about how likely an escape is. Secondary
    /// escape estimation (Bloom's PPMZ, Shkarin's PPMII) pools the evidence of
    /// similar contexts. Each class learns an escape frequency `t`, kept with
    /// [`T_FRAC`] fraction bits, and the escape is coded with weight
    /// `max(1, ⌊t + ½⌋)` against the weights of the visible symbols. A class
    /// starts at the number of visible symbols, which is method D's escape.
    fn escape(&mut self, visible: u32, c: &Node, child: usize) -> (u32, usize) {
        let class = self.escape_class(visible, c, child);
        let t = &mut self.esc_freq[class];
        if *t == 0 {
            *t = visible << T_FRAC;
        }
        (((*t + (1 << (T_FRAC - 1))) >> T_FRAC).max(1), class)
    }

    /// Returns the escape class of context `c`, which has `visible` symbols not
    /// excluded and was reached by escaping from a context of `child` symbols
    /// (0 for the first context tried). The levels and thresholds are ours.
    ///
    /// With exclusions there are 400 classes (§3.3): the visible symbols in 25
    /// levels; whether `c` adds more symbols than the child holds; whether the
    /// next shorter context adds more symbols than `c` adds to the child;
    /// whether the previous byte has either of its top two bits set; and
    /// whether the average count `T(s)/(m(s) + 1)` exceeds 2.
    ///
    /// Without exclusions there are 200, of the kind §5.5 describes, in place
    /// of §3.2's estimate for each context: the symbols in 25 levels; whether
    /// `c` has more than 3/4 as many symbols as the next shorter context; the
    /// previous byte's top bits; and whether a run of likely bytes is under
    /// way.
    fn escape_class(&self, visible: u32, c: &Node, child: usize) -> usize {
        let m = c.len();
        let shorter = if c.suffix == 0 { usize::from(self.order0_len) } else { self.nodes[c.suffix as usize].len() };
        let prev_hi = usize::from((self.hist & 0xFF) >= 0x40);
        if self.any_excluded {
            let level = (visible as usize).clamp(1, 25) - 1;
            let outnumber = usize::from(m.saturating_sub(child) > child);
            let parent_adds_more = usize::from(shorter.saturating_sub(m) > m.saturating_sub(child));
            // T(s) is total/2 in half units: T/(m + 1) > 2 is total > 4(m + 1).
            let busy = usize::from(usize::from(c.total) > 4 * (m + 1));
            level * 16 + outnumber * 8 + parent_adds_more * 4 + prev_hi * 2 + busy
        } else {
            let level = m.clamp(1, 25) - 1;
            let large = usize::from(4 * m > 3 * shorter);
            400 + level * 8 + large * 4 + prev_hi * 2 + usize::from(self.run > MAX_ORDER as u32)
        }
    }

    /// Updates escape class `class` after a decision between an escape and
    /// visible symbols whose weights sum to `sum`. The weights are taken before
    /// this byte is counted.
    ///
    /// The paper says the classes model the escape frequency `t` but not how
    /// they learn it. Ours is the stochastic approximation of Robbins and Monro
    /// (1951) for the mean of an observation that is `sum + t` on an escape and
    /// 0 otherwise: `t += (δ·(sum + t) − t)/N₀`, which adds `sum/N₀` on an
    /// escape and subtracts `t/N₀` on a symbol. `N₀` follows §3.3:
    /// `2^(1+⌈log₂ max(4, N)⌉)` for `N` earlier uses of the class, up to 64.
    /// `t` stays between 1/16 and [`T_MAX`].
    ///
    /// The ideal update settles where `t/(sum + t)` equals the escape rate.
    /// This one only approximates that: the decrement is rounded down, `t` is
    /// bounded, and the coder rounds `t` to a whole weight.
    fn learn_escape(&mut self, class: usize, sum: u32, escaped: bool) {
        let n = &mut self.esc_uses[class];
        let uses = u32::from((*n).max(4));
        *n = n.saturating_add(1);
        let shift = (1 + (32 - (uses - 1).leading_zeros())).min(6);
        let t = &mut self.esc_freq[class];
        *t = if escaped {
            (*t + ((sum << T_FRAC) >> shift)).min(T_MAX << T_FRAC)
        } else {
            (*t - (*t >> shift)).max(1 << (T_FRAC - 4))
        };
    }

    /// Returns how many times the model has restarted, for diagnostics.
    pub fn restarts(&self) -> u32 {
        self.restarts
    }

    /// Codes `byte`.
    ///
    /// The contexts are tried from longest to shortest along the suffix links.
    /// Each divides its interval among its symbols not yet excluded, in stored
    /// order, followed by the escape. If the byte is there, its interval is
    /// coded. Otherwise the escape is coded and the context's symbols are
    /// excluded, since the byte is none of them. A context of one symbol with
    /// nothing excluded codes a binary decision. After the contexts come the
    /// order-0 table and then a uniform choice among the bytes not excluded.
    /// [`Ppm::decode`] makes the same choices.
    pub fn encode<W: Write>(&mut self, enc: &mut MultiRangeEncoder<W>, byte: u8) -> io::Result<()> {
        self.begin_byte();
        let (mut i, mut order) = (self.top, self.top_order);
        let mut first = true;
        // The number of symbols of the context escaped from (0 at first).
        let mut child = 0;
        while i != 0 {
            self.ctx[order] = i;
            let c = self.nodes[i as usize];
            if c.more == 0 {
                if !self.any_excluded {
                    // One symbol: a binary decision, the symbol or escape.
                    let (p1, class) = self.binary_estimate(&c);
                    let hit = c.byte == byte;
                    enc.encode_bit_p(!hit, p1)?;
                    self.learn_binary(class, !hit);
                    if hit {
                        self.escaped_last = !first;
                        let w = BIN_ONE - u32::from(p1);
                        self.update(byte, Found::Ctx { order, at: At::Node(i), w, t: BIN_ONE });
                        return Ok(());
                    }
                    self.exclude(c.byte);
                } else if !self.is_excluded(c.byte) {
                    // One symbol visible among exclusions: coded with an
                    // escape like a larger context.
                    let sum = Self::weight(c.total);
                    let (esc, class) = self.escape(1, &c, child);
                    let total = sum + esc;
                    if c.byte == byte {
                        enc.encode(0, sum, total)?;
                        self.learn_escape(class, sum, false);
                        self.escaped_last = !first;
                        self.update(byte, Found::Ctx { order, at: At::Node(i), w: sum, t: total });
                        return Ok(());
                    }
                    enc.encode(sum, total, total)?;
                    self.learn_escape(class, sum, true);
                    self.exclude(c.byte);
                }
            } else {
                // The byte's interval and slot, the visible total, and the
                // number of visible symbols. With nothing excluded the total
                // is known from the context's, so the scan stops at the byte.
                let (mut low, mut hit, mut at) = (0, 0, 0);
                let (sum, visible) = if !self.any_excluded {
                    for (k, s) in self.symbols(&c).iter().enumerate() {
                        if s.byte == byte {
                            hit = Self::weight(s.count);
                            at = k;
                            break;
                        }
                        low += Self::weight(s.count);
                    }
                    let d = c.len() as u32;
                    (u32::from(c.total) - d, d)
                } else {
                    // The byte escaped from the longer contexts, so it is not
                    // excluded and its weight is not masked.
                    let (mut sum, mut visible) = (0, 0);
                    for (k, s) in self.symbols(&c).iter().enumerate() {
                        let seen = self.seen_mask(s.byte);
                        let w = Self::weight(s.count) & seen;
                        if s.byte == byte {
                            low = sum;
                            hit = w;
                            at = k;
                        }
                        sum += w;
                        visible += seen & 1;
                    }
                    (sum, visible)
                };
                if visible > 0 {
                    let (esc, class) = self.escape(visible, &c, child);
                    let total = sum + esc;
                    if hit > 0 {
                        enc.encode(low, low + hit, total)?;
                        self.learn_escape(class, sum, false);
                        self.escaped_last = !first;
                        let at = At::Slot(c.link + at as u32);
                        self.update(byte, Found::Ctx { order, at, w: hit, t: total });
                        return Ok(());
                    }
                    enc.encode(sum, total, total)?;
                    self.learn_escape(class, sum, true);
                    self.exclude_context(&c);
                }
            }
            i = c.suffix;
            order -= 1;
            first = false;
            child = c.len();
        }
        // Order 0, with method D's escape (the number of visible symbols).
        let (sum, visible) = self.order0_visible();
        if visible > 0 {
            let total = sum + visible;
            let c = self.order0[usize::from(byte)];
            if c > 0 && !self.is_excluded(byte) {
                let mut low = 0;
                for b in 0..byte {
                    let cb = self.order0[usize::from(b)];
                    if cb > 0 && !self.is_excluded(b) {
                        low += Self::weight(cb);
                    }
                }
                enc.encode(low, low + Self::weight(c), total)?;
                self.escaped_last = true;
                self.update(byte, Found::Order0 { w: Self::weight(c), t: total });
                return Ok(());
            }
            enc.encode(sum, total, total)?;
            self.exclude_order0();
        }
        // A byte never seen: uniform over the bytes not excluded.
        let (mut low, mut total) = (0, 0);
        for b in 0..=255u8 {
            if !self.is_excluded(b) {
                if b == byte {
                    low = total;
                }
                total += 1;
            }
        }
        enc.encode(low, low + 1, total)?;
        self.escaped_last = true;
        self.update(byte, Found::Nowhere);
        Ok(())
    }

    /// Decodes one byte. It mirrors [`Ppm::encode`], asking the coder in each
    /// context where the code value falls in the same division of the interval.
    pub fn decode(&mut self, dec: &mut MultiRangeDecoder<'_>) -> io::Result<u8> {
        self.begin_byte();
        let (mut i, mut order) = (self.top, self.top_order);
        let mut first = true;
        // The number of symbols of the context escaped from (0 at first).
        let mut child = 0;
        while i != 0 {
            self.ctx[order] = i;
            let c = self.nodes[i as usize];
            if c.more == 0 {
                if !self.any_excluded {
                    let (p1, class) = self.binary_estimate(&c);
                    let escape = dec.decode_bit_p(p1);
                    self.learn_binary(class, escape);
                    if !escape {
                        self.escaped_last = !first;
                        let w = BIN_ONE - u32::from(p1);
                        self.update(c.byte, Found::Ctx { order, at: At::Node(i), w, t: BIN_ONE });
                        return Ok(c.byte);
                    }
                    self.exclude(c.byte);
                } else if !self.is_excluded(c.byte) {
                    let sum = Self::weight(c.total);
                    let (esc, class) = self.escape(1, &c, child);
                    let total = sum + esc;
                    if dec.target(total) < sum {
                        dec.consume(0, sum, total);
                        self.learn_escape(class, sum, false);
                        self.escaped_last = !first;
                        self.update(c.byte, Found::Ctx { order, at: At::Node(i), w: sum, t: total });
                        return Ok(c.byte);
                    }
                    dec.consume(sum, total, total);
                    self.learn_escape(class, sum, true);
                    self.exclude(c.byte);
                }
            } else {
                let (sum, visible) = self.visible(&c);
                if visible > 0 {
                    let (esc, class) = self.escape(visible, &c, child);
                    let total = sum + esc;
                    let t = dec.target(total);
                    if t < sum {
                        // Find the symbol whose interval holds the target.
                        let (mut low, mut hit) = (0, None);
                        for (k, s) in self.symbols(&c).iter().enumerate() {
                            // An excluded symbol has no interval: weight 0.
                            let w = Self::weight(s.count) & self.seen_mask(s.byte);
                            if t < low + w {
                                hit = Some((s.byte, w, k));
                                break;
                            }
                            low += w;
                        }
                        let (byte, w, at) = hit.expect("target below the visible total");
                        dec.consume(low, low + w, total);
                        self.learn_escape(class, sum, false);
                        self.escaped_last = !first;
                        let at = At::Slot(c.link + at as u32);
                        self.update(byte, Found::Ctx { order, at, w, t: total });
                        return Ok(byte);
                    }
                    dec.consume(sum, total, total);
                    self.learn_escape(class, sum, true);
                    self.exclude_context(&c);
                }
            }
            i = c.suffix;
            order -= 1;
            first = false;
            child = c.len();
        }
        let (sum, visible) = self.order0_visible();
        if visible > 0 {
            let total = sum + visible;
            let t = dec.target(total);
            if t < sum {
                let mut low = 0;
                for b in 0..=255u8 {
                    let c = self.order0[usize::from(b)];
                    if c == 0 || self.is_excluded(b) {
                        continue;
                    }
                    let w = Self::weight(c);
                    if t < low + w {
                        dec.consume(low, low + w, total);
                        self.escaped_last = true;
                        self.update(b, Found::Order0 { w, t: total });
                        return Ok(b);
                    }
                    low += w;
                }
                unreachable!("target below the visible total");
            }
            dec.consume(sum, total, total);
            self.exclude_order0();
        }
        let total = (0..=255u8).filter(|&b| !self.is_excluded(b)).count() as u32;
        if total == 0 {
            // Only a corrupt stream escapes when every byte is excluded.
            return Err(io::Error::new(io::ErrorKind::InvalidData, "corrupt block (escape past every byte)"));
        }
        let t = dec.target(total);
        let b = (0..=255u8).filter(|&b| !self.is_excluded(b)).nth(t as usize).expect("target below total");
        dec.consume(t, t + 1, total);
        self.escaped_last = true;
        self.update(b, Found::Nowhere);
        Ok(b)
    }

    /// Learns `byte`, which was coded as `found` says, and sets the next byte's
    /// longest context.
    ///
    /// The byte is counted in the context that coded it and added to every
    /// longer one. Shorter contexts are left alone but for the half count of
    /// §2.2. The longer contexts that do not exist yet, orders `top_order + 1`
    /// to [`MAX_ORDER`], are created first with their counts pending. Each is
    /// the context one shorter before the previous byte, followed by that byte,
    /// so it is linked from where that byte was learned last time (`learned`).
    /// Right after a restart those places are gone. The new contexts are then
    /// kept as orphans, and each is linked when a symbol that leads to it is
    /// added.
    ///
    /// The next byte's longest context is the one the byte leads to in the
    /// context that coded it, or a longer one if a symbol added now leads to an
    /// orphan. If the nodes or the slots are about to run out, the model
    /// restarts first.
    fn update(&mut self, byte: u8, mut found: Found) {
        let likely = match found {
            Found::Ctx { w, t, .. } | Found::Order0 { w, t } => 2 * w > t,
            Found::Nowhere => false,
        };
        self.run = if likely && !self.escaped_last { self.run + 1 } else { 0 };
        self.prev_likely = likely;
        let top = MAX_ORDER.min(self.seen);
        // Each context may need a new array of up to 256 slots.
        if self.slots.allocated() + top * 256 > self.sym_slots || self.nodes.len() - 1 + top > self.max_contexts {
            self.restart();
            // Nothing is left of the contexts found while coding the byte.
            found = Found::Nowhere;
        }
        if let Found::Ctx { order, at, .. } = found {
            // Shkarin's update of the parent (§2.2): below the longest order,
            // while the symbol's count here is under 8, count it at half
            // weight in the suffix too.
            if updates_parent(order, self.count_at(at)) {
                if order == 1 {
                    self.count_order0(byte, 1);
                } else {
                    let parent = self.nodes[self.ctx[order] as usize].suffix;
                    self.count_half(parent, byte);
                }
            }
        }
        let lowest = match found {
            Found::Ctx { order, .. } => order,
            _ => 1,
        };
        let have = self.top_order;
        for order in have + 1..=top {
            let suffix = if order == 1 { 0 } else { self.ctx[order - 1] };
            let n = self.new_node(suffix, byte);
            self.ctx[order] = n;
            if order == 1 {
                self.order1[(self.hist & 0xFF) as usize] = n;
            } else if self.learned_valid {
                self.link(self.learned[order - 1], n);
                if order == MAX_ORDER && self.seen > MAX_ORDER {
                    // The longest context before the previous byte also
                    // leads here, its oldest byte dropped.
                    self.link(self.learned[MAX_ORDER], n);
                }
            } else {
                self.orphans[self.n_orphans] = (key(self.hist, order), n);
                self.n_orphans += 1;
            }
        }
        // What a byte added to an existing context inherits from (§2.1): its
        // count, and the total, where it was coded.
        let source = match found {
            Found::Ctx { order, at, .. } => Some((self.count_at(at), self.nodes[self.ctx[order] as usize].total)),
            Found::Order0 { .. } => Some((self.order0[usize::from(byte)], self.order0_total as u16)),
            Found::Nowhere => None,
        };
        // The longest context an orphan gives the next byte, if any.
        let mut via_orphan = (0, 0);
        for order in lowest..=top {
            let i = self.ctx[order];
            self.learned[order] = match found {
                Found::Ctx { order: o, at, .. } if o == order => self.count(i, at),
                _ => {
                    // The byte is new here: the context escaped, or was
                    // just created holding it. What the byte leads to: at
                    // the longest order, the same context as one order
                    // shorter (done just before); otherwise nothing yet,
                    // unless an orphan is that context.
                    let next = if order == MAX_ORDER {
                        self.next_of(self.learned[order - 1])
                    } else {
                        let n = self.adopt(key((self.hist << 8) | u64::from(byte), order + 1));
                        if n != 0 {
                            via_orphan = (order + 1, n);
                        }
                        n
                    };
                    if order > have {
                        self.nodes[i as usize].link = next;
                        At::Node(i)
                    } else {
                        let count = match source {
                            Some((h_a, t_k)) => {
                                let c = self.nodes[i as usize];
                                inherited_count(h_a, t_k, c.total, c.len())
                            }
                            None => 2,
                        };
                        self.add(i, byte, next, count)
                    }
                }
            };
        }
        if matches!(found, Found::Order0 { .. } | Found::Nowhere) {
            self.count_order0(byte, 2);
        }
        let (mut next, mut next_order) = match found {
            Found::Ctx { order, .. } => (self.next_of(self.learned[order]), (order + 1).min(MAX_ORDER)),
            _ => (self.order1[usize::from(byte)], 1),
        };
        debug_assert!(next != 0 || !matches!(found, Found::Ctx { .. }), "a coding symbol leads nowhere");
        if via_orphan.0 > next_order {
            (next_order, next) = via_orphan;
        }
        self.top = next;
        self.top_order = if next == 0 { 0 } else { next_order };
        self.learned_valid = true;
        self.hist = (self.hist << 8) | u64::from(byte);
        self.seen += 1;
    }

    /// Points the symbol at `at` to the new context `n`.
    fn link(&mut self, at: At, n: u32) {
        debug_assert!(
            self.next_of(at) == 0 && u64::from(self.byte_at(at)) == self.hist & 0xFF,
            "linking the wrong symbol"
        );
        self.set_next(at, n);
    }

    /// Removes the orphan with this key from the list and returns it, or
    /// returns 0.
    fn adopt(&mut self, key: u64) -> u32 {
        for k in 0..self.n_orphans {
            if self.orphans[k].0 == key {
                let n = self.orphans[k].1;
                self.n_orphans -= 1;
                self.orphans[k] = self.orphans[self.n_orphans];
                return n;
            }
        }
        0
    }

    /// Counts the symbol at `at` in context `i` and returns where it is now. A
    /// symbol that outcounts the one before it moves one place forward, which
    /// keeps frequent symbols near the front of the scan.
    fn count(&mut self, i: u32, at: At) -> At {
        debug_assert!(self.nodes[i as usize].total != 0, "a pending context's count was read");
        let At::Slot(slot) = at else {
            self.add_to_total(i, 2);
            return at;
        };
        let c = self.nodes[i as usize];
        let k = (slot - c.link) as usize;
        let arr = &mut self.slots.slots[c.link as usize..c.link as usize + c.len()];
        arr[k].count += 2;
        let k = if k > 0 && arr[k - 1].count < arr[k].count {
            arr.swap(k - 1, k);
            k - 1
        } else {
            k
        };
        self.add_to_total(i, 2);
        At::Slot(c.link + k as u32)
    }

    /// Adds `byte`, which context `i` does not hold, with `count` half units
    /// and leading to `next`, and returns where it is. A second symbol moves
    /// both into an array of two slots. A full array moves to one of twice the
    /// size, and the old one is freed.
    fn add(&mut self, i: u32, byte: u8, next: u32, count: u16) -> At {
        let c = self.nodes[i as usize];
        let len = c.len();
        let start = if len == 1 {
            debug_assert!(c.byte != byte, "adding a symbol twice");
            let to = self.slots.alloc(2);
            self.slots.slots[to] = Slot { next: c.link, count: c.total, byte: c.byte };
            to
        } else {
            debug_assert!(!self.symbols(&c).iter().any(|s| s.byte == byte), "adding a symbol twice");
            let mut start = c.link as usize;
            if len == capacity(len) {
                let to = self.slots.alloc(capacity(len + 1));
                for k in 0..len {
                    self.slots.slots[to + k] = self.slots.slots[start + k];
                }
                self.slots.release(start, len);
                start = to;
            }
            start
        };
        self.slots.slots[start + len] = Slot { next, count, byte };
        let n = &mut self.nodes[i as usize];
        n.link = start as u32;
        n.more += 1;
        self.add_to_total(i, count);
        At::Slot((start + len) as u32)
    }

    /// Counts `byte` in the order-0 table by `n` half units. Every count is
    /// halved when the total passes [`MAX_TOTAL`].
    fn count_order0(&mut self, byte: u8, n: u16) {
        let c = &mut self.order0[usize::from(byte)];
        if *c == 0 {
            self.order0_len += 1;
        }
        *c += n;
        self.order0_total += u32::from(n);
        if self.order0_total > 2 * MAX_TOTAL {
            self.order0_total = 0;
            for c in &mut self.order0 {
                *c = halve(*c);
                self.order0_total += u32::from(*c);
            }
        }
    }

    /// Counts `byte`, which context `i` holds, at half weight (§2.2). Like a
    /// full count, it may move the symbol one place forward.
    fn count_half(&mut self, i: u32, byte: u8) {
        let c = self.nodes[i as usize];
        debug_assert!(c.total != 0, "a pending context's count was read");
        if c.more > 0 {
            let arr = &mut self.slots.slots[c.link as usize..c.link as usize + c.len()];
            let k = arr.iter().position(|s| s.byte == byte).expect("a suffix holds every symbol of its context");
            arr[k].count += 1;
            if k > 0 && arr[k - 1].count < arr[k].count {
                arr.swap(k - 1, k);
            }
        } else {
            debug_assert_eq!(c.byte, byte, "a suffix holds every symbol of its context");
        }
        self.add_to_total(i, 1);
    }

    /// Adds `n` to the total of context `i`. When the total passes
    /// [`MAX_TOTAL`] every count is halved, so old statistics fade.
    fn add_to_total(&mut self, i: u32, n: u16) {
        let c = &mut self.nodes[i as usize];
        c.total += n;
        if u32::from(c.total) <= 2 * MAX_TOTAL {
            return;
        }
        if c.more == 0 {
            c.total = halve(c.total);
            return;
        }
        let c = *c;
        let mut total = 0;
        for s in &mut self.slots.slots[c.link as usize..c.link as usize + c.len()] {
            s.count = halve(s.count);
            total += s.count;
        }
        self.nodes[i as usize].total = total;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Codes `data` and decodes it with models from `make`, restarting both
    /// before every byte `restart_at` names; the code's length and the
    /// encoder's restarts.
    fn roundtrip(data: &[u8], make: impl Fn() -> Ppm, restart_at: impl Fn(usize) -> bool) -> (usize, u32) {
        let mut m = make();
        let (nodes, slots) = (m.nodes.capacity(), m.slots.slots.capacity());
        let mut enc = MultiRangeEncoder::new(Vec::new());
        for (i, &b) in data.iter().enumerate() {
            if restart_at(i) {
                m.restart();
            }
            m.encode(&mut enc, b).unwrap();
        }
        // The reserved arrays were never outgrown.
        assert_eq!((m.nodes.capacity(), m.slots.slots.capacity()), (nodes, slots));
        let code = enc.finish().unwrap();
        let mut d = make();
        let mut dec = MultiRangeDecoder::new(&code[..]).unwrap();
        for (i, &b) in data.iter().enumerate() {
            if restart_at(i) {
                d.restart();
            }
            assert_eq!(d.decode(&mut dec).unwrap(), b, "byte {i}");
        }
        assert!(dec.is_canonical_end());
        (code.len(), m.restarts)
    }

    /// Text-like data with long repeats, so high-order contexts hit often.
    fn sample() -> Vec<u8> {
        let words = ["context ", "mixing ", "escape ", "Langdon ", "arithmetic ", "coding ", "\n"];
        let mut s = 12345u64;
        let mut v = Vec::new();
        while v.len() < 200_000 {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            v.extend_from_slice(words[(s >> 33) as usize % words.len()].as_bytes());
            if s >> 60 == 0 {
                v.push((s >> 40) as u8);
            }
        }
        v
    }

    fn noise(n: usize) -> Vec<u8> {
        let mut s = 0x9E3779B97F4A7C15u64;
        (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                (s >> 24) as u8
            })
            .collect()
    }

    #[test]
    fn roundtrips() {
        let never = |_| false;
        let data = sample();
        let (len, restarts) = roundtrip(&data, || Ppm::new(data.len()), never);
        assert_eq!(restarts, 0);
        assert!(len < data.len() / 4, "{len}");
        assert_eq!(roundtrip(b"", || Ppm::new(0), never).1, 0);
        assert_eq!(std::mem::size_of::<Node>(), 12);
        assert_eq!(std::mem::size_of::<Slot>(), 8);
        assert_eq!(std::mem::size_of::<At>(), 8);
        let all: Vec<u8> = (0..=255).cycle().take(5000).collect();
        roundtrip(&all, || Ppm::new(all.len()), never);
        // Every context of every order grows: the arena's worst case.
        let random = noise(100_000);
        roundtrip(&random, || Ppm::new(random.len()), never);
    }

    #[test]
    fn roundtrips_across_many_restarts() {
        let data = sample();
        // A tiny arena and context limit: restarts every few hundred bytes,
        // including right after bytes coded in high-order contexts.
        // The context limit is reached first here, then the arena.
        for (syms, contexts) in [(1 << 20, 768), (3200, 49_152)] {
            let (len, restarts) = roundtrip(&data, || Ppm::with_limits(syms, contexts, data.len()), |_| false);
            // A restart empties the model; it must then fill up again
            // rather than restart on every byte.
            assert!(restarts > 20 && (restarts as usize) < data.len() / 50, "{restarts}");
            assert!(len < data.len() / 2, "{len}");
        }
    }

    /// Restarts at chosen bytes: at a fixed period, in bursts, and right
    /// after high-order hits, where the contexts created all at once must
    /// be found again.
    #[test]
    fn roundtrips_with_restarts_anywhere() {
        let data = [sample(), noise(20_000), (0..60_000u32).map(|i| ((i / 97) % 3) as u8).collect()].concat();
        let schedules: [&dyn Fn(usize) -> bool; 3] =
            [&|k| k > 0 && k % 257 == 0, &|k| k % 1000 < 3, &|k| k % 7919 == 6 || k % 7919 == 13];
        for restart_at in schedules {
            roundtrip(&data, || Ppm::new(data.len()), restart_at);
        }
    }

    /// The reservations never pass the limits, whatever the block length,
    /// and reach them for the largest blocks the format allows.
    #[test]
    fn reservations_are_bounded_for_any_block_length() {
        let most = (SYM_SLOTS, 1 + MAX_CONTEXTS + MAX_ORDER);
        for len in [0, 1, 1 << 16, 1 << 24, 179 << 20, 1 << 29, 1 << 30, usize::MAX / 24 + 1, usize::MAX] {
            let (slots, nodes) = Ppm::capacities(SYM_SLOTS, MAX_CONTEXTS, len);
            assert!(slots <= most.0 && nodes <= most.1, "{len}");
            assert!(Ppm::memory(len) <= Ppm::memory(usize::MAX), "{len}");
        }
        assert_eq!(Ppm::capacities(SYM_SLOTS, MAX_CONTEXTS, 1 << 30), most);
        assert_eq!(Ppm::capacities(SYM_SLOTS, MAX_CONTEXTS, usize::MAX), most);
        assert_eq!(Ppm::capacities(SYM_SLOTS, MAX_CONTEXTS, 1000), (24_000, 1 + 6000 + MAX_ORDER));
    }

    /// Equation (5) for a context that holds others, by hand: a byte seen 10
    /// times in a one-symbol context, added to a context with total 20 over
    /// 5 symbols, gets t₀ = 1/4 + 20·(9.5 − 1/4)/(10 − 9.5 + 20 − 5) = 12.19,
    /// so f₀ = 12.69, 25 half units after rounding down at 4·t₀. A byte with
    /// 2 of 100 where it was coded, added to a context with total 10 over 3
    /// symbols, gets t₀ = 0.37 and keeps the least count, 1.
    #[test]
    fn inheritance_follows_equation_five() {
        assert_eq!(inherited_count(20, 20, 40, 5), 25);
        assert_eq!(inherited_count(4, 200, 20, 3), 2);
        assert_eq!(inherited_count(2 * 4000, 2 * 4000, 2 * 4000, 1), 64, "capped at 32");
    }

    /// Equation (5) for a new context by hand: below a parent that gives the
    /// byte weight 9 of 30 half units, t₀ = 1 + 3.5/10.5, 2 half units after
    /// rounding down. A byte of weight 1 (t below τ) gets 1.
    #[test]
    fn a_new_context_follows_equation_five() {
        assert_eq!(new_context_count(9, 30), 2);
        assert_eq!(new_context_count(1, 30), 2);
        assert_eq!(new_context_count(41, 50), 10);
        assert_eq!(new_context_count(900, 901), 64, "capped at 32");
    }

    /// Admission charges every table the model allocates.
    #[test]
    fn memory_counts_every_table() {
        let m = Ppm::new(0);
        let held = std::mem::size_of::<Ppm>()
            + m.nodes.capacity() * std::mem::size_of::<Node>()
            + m.slots.slots.capacity() * std::mem::size_of::<Slot>()
            + (m.esc_uses.capacity() + m.bin_escape.capacity()) * std::mem::size_of::<u16>()
            + m.esc_freq.capacity() * std::mem::size_of::<u32>();
        assert!(Ppm::memory(0) >= held as u64, "{} < {held}", Ppm::memory(0));
    }

    /// A byte coded from the order-0 table with a probability above 1/2
    /// leaves the previous byte marked likely: in "aaaba" the last 'a' is
    /// coded there with probability 4/7.
    #[test]
    fn a_likely_order_zero_byte_is_likely() {
        let mut m = Ppm::new(16);
        let mut enc = MultiRangeEncoder::new(Vec::new());
        for &b in b"aaaba" {
            m.encode(&mut enc, b).unwrap();
        }
        assert!(m.prev_likely);
    }

    /// A class's first estimate is PPMC's 1/(f + 1) for the count f of the
    /// context that first uses it, half counts included: f = 1.5 gives 2/5.
    #[test]
    fn a_class_starts_at_ppmc_for_half_counts_too() {
        let mut m = Ppm::new(16);
        let c = Node { suffix: 0, link: 0, total: 3, more: 0, byte: b'x' };
        let (p, _) = m.binary_estimate(&c);
        assert_eq!(u32::from(p), 2 * BIN_ONE / 5);
    }

    /// The average-count flag of a masked escape class keeps half counts:
    /// two symbols with 6.5 in all average 6.5/3 > 2; with 6, exactly 2.
    #[test]
    fn the_average_count_flag_keeps_half_counts() {
        let mut m = Ppm::new(16);
        m.any_excluded = true;
        for (more, total, busy) in [(1u8, 13u16, 1usize), (1, 12, 0), (8, 41, 1), (8, 40, 0)] {
            let c = Node { suffix: 0, link: 0, total, more, byte: 0 };
            assert_eq!(m.escape_class(2, &c, 0) & 1, busy, "{} symbols, {total} half units", more + 1);
        }
    }

    /// A class's escape frequency settles where the coder's escape
    /// probability t/(T + t) equals the rate of escapes: with T = 30 and one
    /// escape in four, at t = 10.
    #[test]
    fn an_escape_frequency_settles_at_the_escape_rate() {
        let mut m = Ppm::new(16);
        m.esc_freq[0] = 2 << T_FRAC;
        for k in 0..40_000 {
            m.learn_escape(0, 30, k % 4 == 0);
        }
        let t = f64::from(m.esc_freq[0]) / f64::from(1u32 << T_FRAC);
        assert!((t - 10.0).abs() < 1.0, "t = {t}");
    }

    #[test]
    fn the_parent_update_stops_at_a_count_of_eight_in_the_child() {
        assert!(updates_parent(1, 2) && updates_parent(5, 15));
        assert!(!updates_parent(5, 16) && !updates_parent(5, 40));
        assert!(!updates_parent(MAX_ORDER, 2), "not from the longest order");
    }

    /// The parent's update depends on the symbol's count in the context that
    /// coded it, not on its count in the parent: a low child count with a
    /// high parent count updates the parent; a high child count with a low
    /// parent count does not.
    #[test]
    fn the_parent_update_follows_the_child_count() {
        for (child, parent, added) in [(2u16, 40u16, 1u16), (16, 4, 0)] {
            let mut m = Ppm::new(100);
            let mut enc = MultiRangeEncoder::new(Vec::new());
            // After "qz", the order-1 context "q" holds 'z' alone.
            for &b in b"qz" {
                m.encode(&mut enc, b).unwrap();
            }
            let q = m.order1[usize::from(b'q')] as usize;
            assert!(q != 0 && m.nodes[q].more == 0 && m.nodes[q].byte == b'z');
            m.nodes[q].total = child;
            m.order0_total += u32::from(parent) - u32::from(m.order0[usize::from(b'z')]);
            m.order0[usize::from(b'z')] = parent;
            // 'q' is coded from the order-0 table; then 'z' is found in "q",
            // whose suffix is the order-0 table.
            m.encode(&mut enc, b'q').unwrap();
            assert_eq!((m.top_order, m.top as usize), (1, q));
            m.encode(&mut enc, b'z').unwrap();
            assert_eq!(m.order0[usize::from(b'z')], parent + added, "child {child}, parent {parent}");
        }
    }

    /// A new context's count stays pending until the branch reaches it, and
    /// is then set from its parent: after "abab", the order-2 context "ab"
    /// was created holding 'a' and is pending; coding the next byte resolves
    /// it from "b", which holds 'a'.
    #[test]
    fn a_new_context_is_resolved_when_next_encountered() {
        let mut m = Ppm::new(100);
        let mut enc = MultiRangeEncoder::new(Vec::new());
        for &b in b"abab" {
            m.encode(&mut enc, b).unwrap();
        }
        let top = m.top as usize;
        assert!(top != 0 && m.nodes[top].total == 0, "pending before the next byte");
        m.encode(&mut enc, b'a').unwrap();
        assert!(m.nodes[top].total >= 2, "resolved once encountered");
    }

    #[test]
    fn a_corrupt_stream_is_an_error_or_a_wrong_byte_never_a_panic() {
        let data = sample();
        let mut m = Ppm::new(20_000);
        let mut enc = MultiRangeEncoder::new(Vec::new());
        for &b in &data[..20_000] {
            m.encode(&mut enc, b).unwrap();
        }
        let mut code = enc.finish().unwrap();
        for i in (0..code.len()).step_by(97) {
            code[i] ^= 0x5A;
            let mut d = Ppm::new(20_000);
            let Ok(mut dec) = MultiRangeDecoder::new(&code[..]) else { continue };
            for _ in 0..20_000 {
                if d.decode(&mut dec).is_err() {
                    break;
                }
            }
            code[i] ^= 0x5A;
        }
    }
}
