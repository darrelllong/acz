// Copyright (c) 2026 Darrell D. E. Long. All rights reserved.
// Use and redistribution are subject to the BSD 3-clause license, with a
// dedication clause, in the LICENSE file at the root of this repository.
//
// Dedicated to the memory of my colleague Glen G. Langdon

//! PPMd variant H (Shkarin, 2001) with the 7z range coder: the model behind
//! `acz -3`. It follows Igor Pavlov's public-domain `Ppmd7.c`, `Ppmd7Enc.c`
//! and `Ppmd7Dec.c` (7-Zip 23.01) step for step, so for the same order and
//! memory size a block's payload is byte-identical to theirs.
//!
//! **The model.** Each byte is coded in the longest context, of up to
//! `order` preceding bytes, that has seen it; a context that has not codes
//! an escape, its symbols are excluded (masked) from the shorter contexts
//! that follow, and coding moves to the next shorter one. Every byte has a
//! count in the order-0 context, so coding always ends there. A symbol
//! that is found gains 4; when one passes `MAX_FREQ` (124) the context's
//! counts are halved, so recent statistics count for more. Escapes are
//! estimated by secondary escape estimation (SEE): a table of adaptive
//! escape counts indexed by properties of the context. Contexts with a
//! single symbol, the common case at high order, instead code a binary
//! decision with a probability from `bin_summ`, indexed by the symbol's
//! count, the shorter context's size, the previous symbol, whether the last
//! byte was predicted, and the run of such successes. A context created for
//! a new symbol starts with a count inherited from the shorter context that
//! coded it (information inheritance), and after coding, the byte is added
//! to the longer contexts that escaped, but not to the shorter ones (update
//! exclusion).
//!
//! **The structure.** Contexts form a suffix tree: each context points to
//! the context one byte shorter (`suffix`), and each of its symbols points
//! to the context one byte longer (`successor`), so moving from one byte's
//! context to the next needs no search. A successor that has not been
//! built yet points into a copy of the input (the "text"), and the context
//! is created from there when it is first needed.
//!
//! **Memory.** Everything lives in one byte arena of a fixed size, with
//! 32-bit offsets for references (offset 0 is null). The text grows up from
//! the bottom; contexts and symbol arrays are 12-byte units allocated from
//! the top, with free lists by size and occasional gluing of adjacent free
//! blocks. When the arena is full the model restarts from scratch. All of
//! this is Shkarin's design; it determines the output, so it is followed
//! exactly.
//!
//! Records, little-endian in the arena:
//!
//! * state (6 bytes): symbol `u8`, count `u8`, successor `u32`;
//! * context (12 bytes): number of symbols `u16`, then either the sum of
//!   the counts plus the escape count `u16` and the offset of the symbol
//!   array `u32`, or, for a single symbol, that symbol's state in place;
//!   then the suffix `u32`;
//! * free block (12 bytes, while gluing): stamp `u16` (0 = free), units
//!   `u16`, next `u32`; on a free list, just the next offset `u32`.
//!
//! **References.**
//! D. Shkarin, *PPM: One Step to Practicality*, Proc. Data Compression
//! Conference, 2002, pp. 202–211, and the PPMd var. H source (2001, public
//! domain).
//! I. Pavlov, `Ppmd7.c`, `Ppmd7Enc.c`, `Ppmd7Dec.c`, 7-Zip 23.01 (2023,
//! public domain).
//! J. G. Cleary and I. H. Witten, *Data Compression Using Adaptive Coding
//! and Partial String Matching*, IEEE Trans. Commun. 32(4), 1984,
//! pp. 396–402.
//! A. Moffat, *Implementing the PPM Data Compression Scheme*, IEEE Trans.
//! Commun. 38(11), 1990, pp. 1917–1921 (update exclusion).
//! C. Bloom, *Solving the Problems of Context Modeling*, 1998 (SEE).

use std::io;

/// The longest context `acz -3` uses.
pub const ORDER: u32 = 6;

/// The arena size for a block of `len` bytes: 16 bytes per input byte,
/// within 1 MiB and 256 MiB. Encoder and decoder both know `len`.
pub fn arena_size(len: usize) -> u32 {
    (len as u64).saturating_mul(16).clamp(1 << 20, 1 << 28) as u32
}

/// Upper bound on the memory one block of at most `block_size` bytes needs.
pub fn memory(block_size: usize) -> u64 {
    u64::from(arena_size(block_size)) + 4 + std::mem::size_of::<Ppmd>() as u64
}

const MAX_FREQ: u32 = 124;
const UNIT: u32 = 12;
const STATE: u32 = 6;
const INT_BITS: u32 = 7;
const PERIOD_BITS: u32 = 7;
const BIN_SCALE: u32 = 1 << (INT_BITS + PERIOD_BITS);
const N_INDEXES: usize = 4 + 4 + 4 + (128 + 3 - 4 - 2 * 4 - 3 * 4) / 4;
const TOP: u32 = 1 << 24;

const EXP_ESCAPE: [u8; 16] = [25, 14, 9, 7, 5, 5, 4, 4, 4, 3, 3, 3, 2, 2, 2, 2];
const INIT_BIN_ESC: [u16; 8] = [0x3CDD, 0x1F3F, 0x59BF, 0x48F3, 0x64A1, 0x5ABC, 0x6632, 0x6051];

/// A SEE cell: an adaptive escape count `summ`, scaled by `2^shift`, with
/// `count` steps left before `shift` grows (and adaptation slows).
#[derive(Clone, Copy, Default)]
struct See {
    summ: u16,
    shift: u8,
    count: u8,
}

impl See {
    fn update(&mut self) {
        if u32::from(self.shift) < PERIOD_BITS {
            self.count = self.count.wrapping_sub(1);
            if self.count == 0 {
                self.summ = self.summ.wrapping_shl(1);
                self.count = (3u32 << self.shift) as u8;
                self.shift += 1;
            }
        }
    }
}

/// Which SEE cell an escape used; `None` is the dummy cell of the order-0
/// context, which never adapts.
type SeeRef = Option<(usize, usize)>;

/// `((sym + 0xC0) >> 5) & 8`: 8 if the byte is 0x40 or above.
fn hi_bits_3(sym: u32) -> u32 {
    ((sym + 0xC0) >> (8 - 3)) & (1 << 3)
}

fn hi_bits_4(sym: u32) -> u32 {
    ((sym + 0xC0) >> (8 - 4)) & (1 << 4)
}

/// The PPMd variant H model and its arena.
pub struct Ppmd {
    mem: Vec<u8>,
    size: u32,
    align: u32,
    min_ctx: u32,
    max_ctx: u32,
    found: u32,
    order_fall: u32,
    init_esc: u32,
    prev_success: u32,
    max_order: u32,
    hi_bits_flag: u32,
    run_length: i32,
    init_rl: i32,
    glue_count: u32,
    lo_unit: u32,
    hi_unit: u32,
    text: u32,
    units_start: u32,
    indx2units: [u8; N_INDEXES],
    units2indx: [u8; 128],
    free_list: [u32; N_INDEXES],
    ns2bs: [u8; 256],
    ns2indx: [u8; 256],
    see: [[See; 16]; 25],
    dummy_see: See,
    bin_summ: [[u16; 64]; 128],
    mask: [u8; 256],
    mask_gen: u8,
    restarts: u32,
}

impl Ppmd {
    /// A fresh model of the given order (2..=64) with an arena of `size`
    /// bytes (at least 2048).
    pub fn new(max_order: u32, size: u32) -> Self {
        assert!((2..=64).contains(&max_order) && (1 << 11..=u32::MAX - 36).contains(&size));
        let align = 4u32.wrapping_sub(size) & 3;
        let mut p = Ppmd {
            mem: vec![0; (align + size) as usize],
            size,
            align,
            min_ctx: 0,
            max_ctx: 0,
            found: 0,
            order_fall: 0,
            init_esc: 0,
            prev_success: 0,
            max_order,
            hi_bits_flag: 0,
            run_length: 0,
            init_rl: 0,
            glue_count: 0,
            lo_unit: 0,
            hi_unit: 0,
            text: 0,
            units_start: 0,
            indx2units: [0; N_INDEXES],
            units2indx: [0; 128],
            free_list: [0; N_INDEXES],
            ns2bs: [0; 256],
            ns2indx: [0; 256],
            see: [[See::default(); 16]; 25],
            dummy_see: See::default(),
            bin_summ: [[0; 64]; 128],
            mask: [0; 256],
            mask_gen: 0,
            restarts: 0,
        };
        // Block sizes in units: 1, 2, 3, 4, 6, 8, 10, 12, 15, 18, 21, 24,
        // then every 4 up to 128.
        let mut k = 0usize;
        for i in 0..N_INDEXES {
            let step = if i >= 12 { 4 } else { (i >> 2) + 1 };
            for _ in 0..step {
                p.units2indx[k] = i as u8;
                k += 1;
            }
            p.indx2units[i] = k as u8;
        }
        p.ns2bs[0] = 0;
        p.ns2bs[1] = 2;
        p.ns2bs[2..11].fill(4);
        p.ns2bs[11..].fill(6);
        for i in 0..3 {
            p.ns2indx[i] = i as u8;
        }
        let (mut m, mut k) = (3u32, 1u32);
        for i in 3..256 {
            p.ns2indx[i] = m as u8;
            k -= 1;
            if k == 0 {
                m += 1;
                k = m - 2;
            }
        }
        p.restart();
        p.restarts = 0;
        p
    }

    /// How many times the model has restarted because its arena was full.
    pub fn restarts(&self) -> u32 {
        self.restarts
    }

    // ---- arena access ----

    fn b(&self, o: u32) -> u32 {
        u32::from(self.mem[o as usize])
    }
    fn set_b(&mut self, o: u32, v: u32) {
        self.mem[o as usize] = v as u8;
    }
    fn w(&self, o: u32) -> u32 {
        let o = o as usize;
        u32::from(u16::from_le_bytes(self.mem[o..o + 2].try_into().expect("2 bytes")))
    }
    fn set_w(&mut self, o: u32, v: u32) {
        let o = o as usize;
        self.mem[o..o + 2].copy_from_slice(&(v as u16).to_le_bytes());
    }
    fn d(&self, o: u32) -> u32 {
        let o = o as usize;
        u32::from_le_bytes(self.mem[o..o + 4].try_into().expect("4 bytes"))
    }
    fn set_d(&mut self, o: u32, v: u32) {
        let o = o as usize;
        self.mem[o..o + 4].copy_from_slice(&v.to_le_bytes());
    }

    // Context fields.
    fn num_stats(&self, c: u32) -> u32 {
        self.w(c)
    }
    fn summ_freq(&self, c: u32) -> u32 {
        self.w(c + 2)
    }
    fn stats(&self, c: u32) -> u32 {
        self.d(c + 4)
    }
    fn suffix(&self, c: u32) -> u32 {
        self.d(c + 8)
    }
    /// The state of a single-symbol context, stored in place at offset 2.
    fn one_state(c: u32) -> u32 {
        c + 2
    }

    // State fields.
    fn sym(&self, s: u32) -> u32 {
        self.b(s)
    }
    fn freq(&self, s: u32) -> u32 {
        self.b(s + 1)
    }
    fn successor(&self, s: u32) -> u32 {
        self.d(s + 2)
    }
    fn state(&self, s: u32) -> [u8; 6] {
        let s = s as usize;
        self.mem[s..s + 6].try_into().expect("6 bytes")
    }
    fn set_state(&mut self, s: u32, v: [u8; 6]) {
        let s = s as usize;
        self.mem[s..s + 6].copy_from_slice(&v);
    }
    fn swap_with_previous(&mut self, s: u32) {
        let (a, b) = (self.state(s), self.state(s - STATE));
        self.set_state(s, b);
        self.set_state(s - STATE, a);
    }
    fn copy_units(&mut self, dest: u32, src: u32, units: u32) {
        let n = (units * UNIT) as usize;
        self.mem.copy_within(src as usize..src as usize + n, dest as usize);
    }

    fn i2u(&self, i: usize) -> u32 {
        u32::from(self.indx2units[i])
    }
    fn u2i(&self, nu: u32) -> usize {
        usize::from(self.units2indx[nu as usize - 1])
    }

    // ---- the suballocator (Ppmd7.c, "Internal Memory Allocator") ----

    fn insert_node(&mut self, node: u32, indx: usize) {
        self.set_d(node, self.free_list[indx]);
        self.free_list[indx] = node;
    }

    fn remove_node(&mut self, indx: usize) -> u32 {
        let node = self.free_list[indx];
        self.free_list[indx] = self.d(node);
        node
    }

    /// Return the tail of a block of class `old` beyond class `new` to the
    /// free lists.
    fn split_block(&mut self, ptr: u32, old: usize, new: usize) {
        let nu = self.i2u(old) - self.i2u(new);
        let ptr = ptr + self.i2u(new) * UNIT;
        let mut i = self.u2i(nu);
        if self.i2u(i) != nu {
            i -= 1;
            let k = self.i2u(i);
            self.insert_node(ptr + k * UNIT, (nu - k - 1) as usize);
        }
        self.insert_node(ptr, i);
    }

    /// Merge adjacent free blocks and rebuild the free lists. A unit's first
    /// 16 bits are never 0 in a live record (a context's symbol count, or a
    /// state's symbol and nonzero count), so 0 marks a free block; a stamp
    /// of 1 at `lo_unit` stops the walk at the unallocated gap.
    fn glue_free_blocks(&mut self) {
        self.glue_count = 255;
        if self.lo_unit != self.hi_unit {
            self.set_w(self.lo_unit, 1);
        }
        // Chain every free block, recording its size.
        let mut n = 0u32;
        for i in 0..N_INDEXES {
            let nu = self.i2u(i);
            let mut next = self.free_list[i];
            self.free_list[i] = 0;
            while next != 0 {
                let node = next;
                next = self.d(node);
                self.set_w(node, 0);
                self.set_w(node + 2, nu);
                self.set_d(node + 4, n);
                n = node;
            }
        }
        let mut head = n;
        // Glue each block to the free blocks that follow it in memory.
        // `prev` is where the link to the current block is kept: the head
        // (None) or a node's next field.
        let mut prev: Option<u32> = None;
        while n != 0 {
            let node = n;
            let mut nu = self.w(node + 2);
            n = self.d(node + 4);
            if nu == 0 {
                match prev {
                    None => head = n,
                    Some(pn) => self.set_d(pn + 4, n),
                }
                continue;
            }
            prev = Some(node);
            loop {
                let node2 = node + nu * UNIT;
                nu += self.w(node2 + 2);
                if self.w(node2) != 0 || nu >= 0x10000 {
                    break;
                }
                self.set_w(node + 2, nu);
                self.set_w(node2 + 2, 0);
            }
        }
        // Refill the free lists, cutting large blocks into classes.
        n = head;
        while n != 0 {
            let mut node = n;
            let mut nu = self.w(node + 2);
            n = self.d(node + 4);
            if nu == 0 {
                continue;
            }
            while nu > 128 {
                self.insert_node(node, N_INDEXES - 1);
                nu -= 128;
                node += 128 * UNIT;
            }
            let mut i = self.u2i(nu);
            if self.i2u(i) != nu {
                i -= 1;
                let k = self.i2u(i);
                self.insert_node(node + k * UNIT, (nu - k - 1) as usize);
            }
            self.insert_node(node, i);
        }
    }

    /// Allocate when the free list and the gap are empty: glue (now and
    /// then), take a larger free block and split it, or take units from
    /// the top of the text area. Returns 0 if memory is exhausted.
    fn alloc_units_rare(&mut self, indx: usize) -> u32 {
        if self.glue_count == 0 {
            self.glue_free_blocks();
            if self.free_list[indx] != 0 {
                return self.remove_node(indx);
            }
        }
        let mut i = indx;
        loop {
            i += 1;
            if i == N_INDEXES {
                let bytes = self.i2u(indx) * UNIT;
                let us = self.units_start;
                self.glue_count = self.glue_count.wrapping_sub(1);
                return if us - self.text > bytes {
                    self.units_start = us - bytes;
                    self.units_start
                } else {
                    0
                };
            }
            if self.free_list[i] != 0 {
                break;
            }
        }
        let block = self.remove_node(i);
        self.split_block(block, i, indx);
        block
    }

    fn alloc_units(&mut self, indx: usize) -> u32 {
        if self.free_list[indx] != 0 {
            return self.remove_node(indx);
        }
        let bytes = self.i2u(indx) * UNIT;
        let lo = self.lo_unit;
        if self.hi_unit - lo >= bytes {
            self.lo_unit = lo + bytes;
            return lo;
        }
        self.alloc_units_rare(indx)
    }

    /// Start over with an empty arena: an order-0 context holding every byte
    /// once, and the initial binary and SEE estimates.
    fn restart(&mut self) {
        self.restarts += 1;
        self.free_list = [0; N_INDEXES];
        self.text = self.align;
        self.hi_unit = self.text + self.size;
        self.lo_unit = self.hi_unit - self.size / 8 / UNIT * 7 * UNIT;
        self.units_start = self.lo_unit;
        self.glue_count = 0;
        self.order_fall = self.max_order;
        self.init_rl = -(self.max_order.min(12) as i32) - 1;
        self.run_length = self.init_rl;
        self.prev_success = 0;

        self.hi_unit -= UNIT;
        let mc = self.hi_unit;
        let s = self.lo_unit;
        self.lo_unit += 128 * UNIT;
        self.max_ctx = mc;
        self.min_ctx = mc;
        self.found = s;
        self.set_w(mc, 256);
        self.set_w(mc + 2, 257);
        self.set_d(mc + 4, s);
        self.set_d(mc + 8, 0);
        for i in 0..256 {
            let st = s + i * STATE;
            self.set_state(st, [i as u8, 1, 0, 0, 0, 0]);
        }
        for (i, row) in self.bin_summ.iter_mut().enumerate() {
            for (k, &esc) in INIT_BIN_ESC.iter().enumerate() {
                let v = (BIN_SCALE - u32::from(esc) / (i as u32 + 2)) as u16;
                for m in (0..64).step_by(8) {
                    row[k + m] = v;
                }
            }
        }
        for (i, row) in self.see.iter_mut().enumerate() {
            let summ = ((5 * i as u32 + 10) << (PERIOD_BITS - 4)) as u16;
            for s in row.iter_mut() {
                *s = See { summ, shift: (PERIOD_BITS - 4) as u8, count: 4 };
            }
        }
        self.dummy_see = See { summ: 0, shift: PERIOD_BITS as u8, count: 64 };
    }

    // ---- model update (Ppmd7.c) ----

    /// Build the contexts that `found`'s successor still points to in the
    /// text: one single-symbol context per order from the first existing
    /// one up. Returns the longest, or 0 if memory ran out.
    fn create_successors(&mut self) -> u32 {
        let mut c = self.min_ctx;
        let mut up_branch = self.successor(self.found);
        let mut ps = [0u32; 64];
        let mut n = 0usize;
        if self.order_fall != 0 {
            ps[n] = self.found;
            n += 1;
        }
        while self.suffix(c) != 0 {
            c = self.suffix(c);
            let s = if self.num_stats(c) != 1 {
                let sym = self.sym(self.found);
                let mut s = self.stats(c);
                while self.sym(s) != sym {
                    s += STATE;
                }
                s
            } else {
                Self::one_state(c)
            };
            let succ = self.successor(s);
            if succ != up_branch {
                c = succ;
                if n == 0 {
                    return c;
                }
                break;
            }
            ps[n] = s;
            n += 1;
        }
        let new_sym = self.b(up_branch);
        up_branch += 1;
        // Information inheritance: the new contexts start with a count
        // that reflects how confident the parent was about this symbol.
        let new_freq = if self.num_stats(c) == 1 {
            self.freq(Self::one_state(c))
        } else {
            let mut s = self.stats(c);
            while self.sym(s) != new_sym {
                s += STATE;
            }
            let cf = self.freq(s) - 1;
            let s0 = self.summ_freq(c).wrapping_sub(self.num_stats(c)).wrapping_sub(cf);
            let f = if 2 * cf <= s0 { u32::from(5 * cf > s0) } else { (2 * cf + s0 - 1) / (2 * s0) + 1 };
            (1 + f) & 0xFF
        };
        loop {
            let c1 = if self.hi_unit != self.lo_unit {
                self.hi_unit -= UNIT;
                self.hi_unit
            } else if self.free_list[0] != 0 {
                self.remove_node(0)
            } else {
                let r = self.alloc_units_rare(0);
                if r == 0 {
                    return 0;
                }
                r
            };
            self.set_w(c1, 1);
            let os = Self::one_state(c1);
            self.set_b(os, new_sym);
            self.set_b(os + 1, new_freq);
            self.set_d(os + 2, up_branch);
            self.set_d(c1 + 8, c);
            n -= 1;
            self.set_d(ps[n] + 2, c1);
            c = c1;
            if n == 0 {
                return c;
            }
        }
    }

    /// After coding `found`: update the shorter context's count, move to
    /// the next byte's context (building it if needed), and add the byte to
    /// every longer context that escaped.
    fn update_model(&mut self) {
        let fs_freq = self.freq(self.found);
        let fs_sym = self.sym(self.found);
        if fs_freq < MAX_FREQ / 4 && self.suffix(self.min_ctx) != 0 {
            let c = self.suffix(self.min_ctx);
            if self.num_stats(c) == 1 {
                let s = Self::one_state(c);
                if self.freq(s) < 32 {
                    self.set_b(s + 1, self.freq(s) + 1);
                }
            } else {
                let mut s = self.stats(c);
                if self.sym(s) != fs_sym {
                    loop {
                        s += STATE;
                        if self.sym(s) == fs_sym {
                            break;
                        }
                    }
                    if self.freq(s) >= self.freq(s - STATE) {
                        self.swap_with_previous(s);
                        s -= STATE;
                    }
                }
                if self.freq(s) < MAX_FREQ - 9 {
                    self.set_b(s + 1, self.freq(s) + 2);
                    self.set_w(c + 2, self.summ_freq(c) + 2);
                }
            }
        }

        if self.order_fall == 0 {
            let c = self.create_successors();
            self.min_ctx = c;
            self.max_ctx = c;
            if c == 0 {
                self.restart();
                return;
            }
            self.set_d(self.found + 2, c);
            return;
        }

        self.set_b(self.text, fs_sym);
        self.text += 1;
        if self.text >= self.units_start {
            self.restart();
            return;
        }
        let mut max_successor = self.text;
        let mut min_successor = self.successor(self.found);

        if min_successor != 0 {
            if min_successor <= max_successor {
                // A successor into the text: build the real contexts.
                let cs = self.create_successors();
                if cs == 0 {
                    self.restart();
                    return;
                }
                min_successor = cs;
            }
            self.order_fall -= 1;
            if self.order_fall == 0 {
                max_successor = min_successor;
                if self.max_ctx != self.min_ctx {
                    self.text -= 1;
                }
            }
        } else {
            // Only the order-0 context has null successors.
            self.set_d(self.found + 2, max_successor);
            min_successor = self.min_ctx;
        }

        let mc = self.min_ctx;
        let mut c = self.max_ctx;
        self.max_ctx = min_successor;
        self.min_ctx = min_successor;
        if c == mc {
            return;
        }

        let ns = self.num_stats(mc);
        let s0 = self.summ_freq(mc).wrapping_sub(ns).wrapping_sub(self.freq(self.found) - 1);
        loop {
            let ns1 = self.num_stats(c);
            let mut sum;
            if ns1 != 1 {
                if ns1 & 1 == 0 {
                    // The array is full: grow it by one unit.
                    let old_nu = ns1 >> 1;
                    let i = self.u2i(old_nu);
                    if i != self.u2i(old_nu + 1) {
                        let ptr = self.alloc_units(i + 1);
                        if ptr == 0 {
                            self.restart();
                            return;
                        }
                        let old = self.stats(c);
                        self.copy_units(ptr, old, old_nu);
                        self.insert_node(old, i);
                        self.set_d(c + 4, ptr);
                    }
                }
                sum = self.summ_freq(c);
                sum += u32::from(2 * ns1 < ns) + 2 * (u32::from(4 * ns1 <= ns) & u32::from(sum <= 8 * ns1));
            } else {
                // A single-symbol context becomes a two-symbol one.
                let s = self.alloc_units(0);
                if s == 0 {
                    self.restart();
                    return;
                }
                let mut freq = self.b(c + 3);
                self.set_b(s, self.b(c + 2));
                self.set_d(s + 2, self.d(c + 4));
                self.set_d(c + 4, s);
                freq = if freq < MAX_FREQ / 4 - 1 { freq << 1 } else { MAX_FREQ - 4 };
                self.set_b(s + 1, freq);
                sum = freq + self.init_esc + u32::from(ns > 3);
            }
            let s = self.stats(c) + ns1 * STATE;
            let mut cf = 2 * (sum + 6) * self.freq(self.found);
            let sf = s0.wrapping_add(sum);
            self.set_b(s, fs_sym);
            self.set_w(c, ns1 + 1);
            self.set_d(s + 2, max_successor);
            if cf < 6 * sf {
                cf = 1 + u32::from(cf > sf) + u32::from(cf >= 4 * sf);
                sum += 3;
            } else {
                cf = 4 + u32::from(cf >= 9 * sf) + u32::from(cf >= 12 * sf) + u32::from(cf >= 15 * sf);
                sum += cf;
            }
            self.set_w(c + 2, sum);
            self.set_b(s + 1, cf);
            c = self.suffix(c);
            if c == mc {
                return;
            }
        }
    }

    /// Halve the counts of `min_ctx` (keeping them sorted, most frequent
    /// first) and drop symbols that reach 0, which only a context of the
    /// highest order may do.
    fn rescale(&mut self) {
        let stats = self.stats(self.min_ctx);
        let mut s = self.found;
        if s != stats {
            let tmp = self.state(s);
            while s != stats {
                self.set_state(s, self.state(s - STATE));
                s -= STATE;
            }
            self.set_state(s, tmp);
        }
        let mut sum_freq = self.freq(s);
        let mut esc_freq = self.summ_freq(self.min_ctx).wrapping_sub(sum_freq);
        let adder = u32::from(self.order_fall != 0);
        sum_freq = (sum_freq + 4 + adder) >> 1;
        let mut i = self.num_stats(self.min_ctx) - 1;
        self.set_b(s + 1, sum_freq);
        loop {
            s += STATE;
            let mut freq = self.freq(s);
            esc_freq = esc_freq.wrapping_sub(freq);
            freq = (freq + adder) >> 1;
            sum_freq += freq;
            self.set_b(s + 1, freq);
            if freq > self.freq(s - STATE) {
                let tmp = self.state(s);
                let mut s1 = s;
                loop {
                    self.set_state(s1, self.state(s1 - STATE));
                    s1 -= STATE;
                    if s1 == stats || freq <= self.freq(s1 - STATE) {
                        break;
                    }
                }
                self.set_state(s1, tmp);
            }
            i -= 1;
            if i == 0 {
                break;
            }
        }
        if self.freq(s) == 0 {
            let mut removed = 0;
            loop {
                removed += 1;
                s -= STATE;
                if self.freq(s) != 0 {
                    break;
                }
            }
            esc_freq += removed;
            let mc = self.min_ctx;
            let num_stats = self.num_stats(mc);
            let new_stats = num_stats - removed;
            self.set_w(mc, new_stats);
            let n0 = (num_stats + 1) >> 1;
            if new_stats == 1 {
                let mut freq = self.freq(stats);
                loop {
                    esc_freq >>= 1;
                    freq = (freq + 1) >> 1;
                    if esc_freq <= 1 {
                        break;
                    }
                }
                let os = Self::one_state(mc);
                self.set_state(os, self.state(stats));
                self.set_b(os + 1, freq);
                self.found = os;
                let i0 = self.u2i(n0);
                self.insert_node(stats, i0);
                return;
            }
            let n1 = (new_stats + 1) >> 1;
            if n0 != n1 {
                let (i0, i1) = (self.u2i(n0), self.u2i(n1));
                if i0 != i1 {
                    if self.free_list[i1] != 0 {
                        let ptr = self.remove_node(i1);
                        self.set_d(self.min_ctx + 4, ptr);
                        self.copy_units(ptr, stats, n1);
                        self.insert_node(stats, i0);
                    } else {
                        self.split_block(stats, i0, i1);
                    }
                }
            }
        }
        let mc = self.min_ctx;
        self.set_w(mc + 2, sum_freq + esc_freq - (esc_freq >> 1));
        self.found = self.stats(mc);
    }

    /// The SEE cell for an escape from `min_ctx` with `num_masked` symbols
    /// excluded, and the escape count it gives.
    fn make_esc_freq(&mut self, num_masked: u32) -> (SeeRef, u32) {
        let mc = self.min_ctx;
        let num_stats = self.num_stats(mc);
        if num_stats == 256 {
            return (None, 1);
        }
        let non_masked = num_stats - num_masked;
        let row = usize::from(self.ns2indx[non_masked as usize - 1]);
        let col = u32::from(non_masked < self.num_stats(self.suffix(mc)).wrapping_sub(num_stats))
            + 2 * u32::from(self.summ_freq(mc) < 11 * num_stats)
            + 4 * u32::from(num_masked > non_masked)
            + self.hi_bits_flag;
        let see = &mut self.see[row][col as usize];
        let summ = u32::from(see.summ);
        let r = summ >> see.shift;
        see.summ = (summ - r) as u16;
        (Some((row, col as usize)), r + u32::from(r == 0))
    }

    fn see_mut(&mut self, see: SeeRef) -> &mut See {
        match see {
            Some((r, c)) => &mut self.see[r][c],
            None => &mut self.dummy_see,
        }
    }

    /// The `bin_summ` cell for the single-symbol `min_ctx`. Also sets
    /// `hi_bits_flag` from the previous byte, as the reference does.
    fn bin_index(&mut self) -> (usize, usize) {
        let os = Self::one_state(self.min_ctx);
        let suffix_ns = self.num_stats(self.suffix(self.min_ctx));
        self.hi_bits_flag = hi_bits_3(self.sym(self.found));
        let col = self.prev_success
            + ((self.run_length >> 26) & 0x20) as u32
            + u32::from(self.ns2bs[suffix_ns as usize - 1])
            + hi_bits_4(self.sym(os))
            + self.hi_bits_flag;
        (self.freq(os) as usize - 1, col as usize)
    }

    fn next_context(&mut self) {
        let c = self.successor(self.found);
        if self.order_fall == 0 && c > self.text {
            self.max_ctx = c;
            self.min_ctx = c;
        } else {
            self.update_model();
        }
    }

    /// After a symbol other than the first was found in `min_ctx`.
    fn update1(&mut self) {
        let mut s = self.found;
        let freq = self.freq(s) + 4;
        self.set_w(self.min_ctx + 2, self.summ_freq(self.min_ctx) + 4);
        self.set_b(s + 1, freq);
        if freq > self.freq(s - STATE) {
            self.swap_with_previous(s);
            s -= STATE;
            self.found = s;
            if freq > MAX_FREQ {
                self.rescale();
            }
        }
        self.next_context();
    }

    /// After the first (most frequent) symbol was found in `min_ctx`.
    fn update1_0(&mut self) {
        let s = self.found;
        let mc = self.min_ctx;
        let freq = self.freq(s);
        let summ = self.summ_freq(mc);
        self.prev_success = u32::from(2 * freq > summ);
        self.run_length += self.prev_success as i32;
        self.set_w(mc + 2, summ + 4);
        self.set_b(s + 1, freq + 4);
        if freq + 4 > MAX_FREQ {
            self.rescale();
        }
        self.next_context();
    }

    /// After a symbol was found in a shorter context, following escapes.
    fn update2(&mut self) {
        let s = self.found;
        let freq = self.freq(s) + 4;
        self.run_length = self.init_rl;
        self.set_w(self.min_ctx + 2, self.summ_freq(self.min_ctx) + 4);
        self.set_b(s + 1, freq);
        if freq > MAX_FREQ {
            self.rescale();
        }
        self.update_model();
    }

    /// After the symbol of a single-symbol context was found.
    fn update_bin(&mut self, s: u32) {
        let freq = self.freq(s);
        let c = self.successor(s);
        self.found = s;
        self.prev_success = 1;
        self.run_length += 1;
        self.set_b(s + 1, freq + u32::from(freq < 128));
        if self.order_fall == 0 && c > self.text {
            self.max_ctx = c;
            self.min_ctx = c;
        } else {
            self.update_model();
        }
    }

    /// Start excluding symbols for a new byte. A symbol is excluded when its
    /// mask entry equals the current generation, so starting over is one
    /// increment; the array is cleared only when the generation wraps.
    fn begin_mask(&mut self) {
        self.mask_gen = self.mask_gen.wrapping_add(1);
        if self.mask_gen == 0 {
            self.mask = [0; 256];
            self.mask_gen = 1;
        }
    }

    fn exclude(&mut self, sym: u32) {
        self.mask[sym as usize] = self.mask_gen;
    }

    /// The count of state `s`, or 0 if its symbol is excluded.
    fn unmasked_freq(&self, s: u32) -> u32 {
        // Symbol and count in one load; no branch on the mask, which is
        // unpredictable.
        let sf = self.w(s);
        let kept = u32::from(self.mask[(sf & 0xFF) as usize] != self.mask_gen);
        (sf >> 8) & kept.wrapping_neg()
    }

    /// Exclude every symbol of `min_ctx` from the shorter contexts.
    fn mask_all(&mut self) {
        let s = self.stats(self.min_ctx);
        for k in 0..self.num_stats(self.min_ctx) {
            self.exclude(self.sym(s + k * STATE));
        }
    }

    /// The sum of the counts of the symbols of `min_ctx` not excluded.
    fn unmasked_sum(&self) -> u32 {
        let s = self.stats(self.min_ctx);
        (0..self.num_stats(self.min_ctx)).map(|k| self.unmasked_freq(s + k * STATE)).sum()
    }

    /// Move to the next shorter context with more symbols than the
    /// `num_masked` already excluded. False if there is none.
    fn escape_to_shorter(&mut self) -> bool {
        let num_masked = self.num_stats(self.min_ctx);
        let mut mc = self.min_ctx;
        loop {
            self.order_fall += 1;
            if self.suffix(mc) == 0 {
                return false;
            }
            mc = self.suffix(mc);
            if self.num_stats(mc) != num_masked {
                break;
            }
        }
        self.min_ctx = mc;
        true
    }

    /// Code `byte`.
    // Kept out of line: inlined into the block loop, its speed swung by up
    // to 50% with unrelated changes elsewhere in the crate (code placement).
    #[inline(never)]
    pub fn encode(&mut self, rc: &mut RangeEncoder, byte: u8) {
        let symbol = u32::from(byte);
        let mc = self.min_ctx;
        if self.num_stats(mc) != 1 {
            let mut s = self.stats(mc);
            rc.range /= self.summ_freq(mc);
            if self.sym(s) == symbol {
                rc.encode(0, self.freq(s));
                rc.normalize();
                self.found = s;
                self.update1_0();
                return;
            }
            self.prev_success = 0;
            let mut sum = self.freq(s);
            for _ in 1..self.num_stats(mc) {
                s += STATE;
                if self.sym(s) == symbol {
                    rc.encode(sum, self.freq(s));
                    rc.normalize();
                    self.found = s;
                    self.update1();
                    return;
                }
                sum += self.freq(s);
            }
            rc.encode(sum, self.summ_freq(mc) - sum);
            self.hi_bits_flag = hi_bits_3(self.sym(self.found));
            self.begin_mask();
            self.mask_all();
        } else {
            let (bi, bj) = self.bin_index();
            let s = Self::one_state(mc);
            let pr = u32::from(self.bin_summ[bi][bj]);
            let bound = (rc.range >> 14) * pr;
            let pr = pr - ((pr + (1 << (PERIOD_BITS - 2))) >> PERIOD_BITS);
            if self.sym(s) == symbol {
                self.bin_summ[bi][bj] = (pr + (1 << INT_BITS)) as u16;
                rc.range = bound;
                rc.normalize_once();
                self.update_bin(s);
                return;
            }
            self.bin_summ[bi][bj] = pr as u16;
            self.init_esc = u32::from(EXP_ESCAPE[(pr >> 10) as usize]);
            rc.low += u64::from(bound);
            rc.range -= bound;
            self.begin_mask();
            self.exclude(self.sym(s));
            self.prev_success = 0;
        }
        loop {
            rc.normalize();
            let num_masked = self.num_stats(self.min_ctx);
            if !self.escape_to_shorter() {
                return;
            }
            let (see, esc) = self.make_esc_freq(num_masked);
            let stats = self.stats(self.min_ctx);
            let n = self.num_stats(self.min_ctx);
            let mut sum = 0;
            for k in 0..n {
                let s = stats + k * STATE;
                let cur = self.sym(s);
                if cur == symbol {
                    let freq = self.freq(s);
                    self.see_mut(see).update();
                    self.found = s;
                    // The total needs the counts after this symbol too.
                    let rest: u32 = (k + 1..n).map(|j| self.unmasked_freq(stats + j * STATE)).sum();
                    let total = sum + freq + rest + esc;
                    rc.range /= total;
                    rc.encode(sum, freq);
                    rc.normalize();
                    self.update2();
                    return;
                }
                sum += self.unmasked_freq(s);
            }
            let total = sum + esc;
            let cell = self.see_mut(see);
            cell.summ = cell.summ.wrapping_add(total as u16);
            rc.range /= total;
            rc.encode(sum, esc);
            self.mask_all();
        }
    }

    /// Decode one byte, or `None` if the code string is invalid.
    // Out of line for the same reason as `encode`.
    #[inline(never)]
    pub fn decode(&mut self, rc: &mut RangeDecoder) -> Option<u8> {
        let mc = self.min_ctx;
        if self.num_stats(mc) != 1 {
            let mut s = self.stats(mc);
            let summ = self.summ_freq(mc);
            let mut count = rc.threshold(summ);
            let hi = count;
            count = count.wrapping_sub(self.freq(s));
            if (count as i32) < 0 {
                rc.decode(0, self.freq(s));
                rc.normalize();
                self.found = s;
                let sym = self.sym(s) as u8;
                self.update1_0();
                return Some(sym);
            }
            self.prev_success = 0;
            for _ in 1..self.num_stats(mc) {
                s += STATE;
                count = count.wrapping_sub(self.freq(s));
                if (count as i32) < 0 {
                    rc.decode(hi.wrapping_sub(count).wrapping_sub(self.freq(s)), self.freq(s));
                    rc.normalize();
                    self.found = s;
                    let sym = self.sym(s) as u8;
                    self.update1();
                    return Some(sym);
                }
            }
            if hi >= summ {
                return None;
            }
            let low = hi.wrapping_sub(count);
            rc.decode(low, summ - low);
            self.hi_bits_flag = hi_bits_3(self.sym(self.found));
            self.begin_mask();
            self.mask_all();
        } else {
            let s = Self::one_state(mc);
            let (bi, bj) = self.bin_index();
            let pr = u32::from(self.bin_summ[bi][bj]);
            let size0 = (rc.range >> 14) * pr;
            let pr = pr - ((pr + (1 << (PERIOD_BITS - 2))) >> PERIOD_BITS);
            if rc.code < size0 {
                self.bin_summ[bi][bj] = (pr + (1 << INT_BITS)) as u16;
                rc.range = size0;
                rc.normalize_once();
                let sym = self.sym(s) as u8;
                self.update_bin(s);
                return Some(sym);
            }
            self.bin_summ[bi][bj] = pr as u16;
            self.init_esc = u32::from(EXP_ESCAPE[(pr >> 10) as usize]);
            rc.code = rc.code.wrapping_sub(size0);
            rc.range -= size0;
            self.begin_mask();
            self.exclude(self.sym(s));
            self.prev_success = 0;
        }
        loop {
            rc.normalize();
            let num_masked = self.num_stats(self.min_ctx);
            if !self.escape_to_shorter() {
                return None;
            }
            let hi = self.unmasked_sum();
            let (see, esc) = self.make_esc_freq(num_masked);
            let freq_sum = esc + hi;
            let mut count = rc.threshold(freq_sum);
            if count < hi {
                let stats = self.stats(self.min_ctx);
                let start = count;
                let mut s = stats;
                loop {
                    count = count.wrapping_sub(self.unmasked_freq(s));
                    s += STATE;
                    if (count as i32) < 0 {
                        break;
                    }
                }
                s -= STATE;
                rc.decode(start.wrapping_sub(count).wrapping_sub(self.freq(s)), self.freq(s));
                rc.normalize();
                self.see_mut(see).update();
                self.found = s;
                let sym = self.sym(s) as u8;
                self.update2();
                return Some(sym);
            }
            if count >= freq_sum {
                return None;
            }
            rc.decode(hi, freq_sum - hi);
            let cell = self.see_mut(see);
            cell.summ = cell.summ.wrapping_add(freq_sum as u16);
            self.mask_all();
        }
    }
}

/// The 7z range coder's encoder, as `Ppmd7z_RangeEnc` uses it: `range` is
/// divided by the total before each `encode`, and normalisation shifts out
/// a byte at a time, carrying into a cached byte and a run of 0xFF bytes.
/// Output stops at `limit` bytes, and its buffer never requests capacity
/// beyond that (the allocator may round up); `full` then says so.
pub struct RangeEncoder {
    low: u64,
    range: u32,
    cache: u8,
    cache_size: u64,
    out: Vec<u8>,
    limit: usize,
    full: bool,
}

impl RangeEncoder {
    /// An encoder that stops writing after `limit` bytes.
    pub fn new(limit: usize) -> Self {
        RangeEncoder { low: 0, range: u32::MAX, cache: 0, cache_size: 1, out: Vec::new(), limit, full: false }
    }

    fn put(&mut self, b: u8) {
        if self.out.len() < self.limit {
            // Grow geometrically, but never ask for more than `limit`.
            let len = self.out.len();
            if len == self.out.capacity() {
                crate::models::reserve_bounded(&mut self.out, len + 1, self.limit);
            }
            self.out.push(b);
        } else {
            self.full = true;
        }
    }

    fn shift_low(&mut self) {
        if (self.low as u32) < 0xFF00_0000 || (self.low >> 32) != 0 {
            let carry = (self.low >> 32) as u8;
            let mut temp = self.cache;
            loop {
                self.put(temp.wrapping_add(carry));
                temp = 0xFF;
                self.cache_size -= 1;
                if self.cache_size == 0 {
                    break;
                }
            }
            self.cache = ((self.low as u32) >> 24) as u8;
        }
        self.cache_size += 1;
        self.low = u64::from((self.low as u32) << 8);
    }

    fn encode(&mut self, start: u32, size: u32) {
        self.low += u64::from(start) * u64::from(self.range);
        self.range *= size;
    }

    fn normalize_once(&mut self) {
        if self.range < TOP {
            self.range <<= 8;
            self.shift_low();
        }
    }

    fn normalize(&mut self) {
        if self.range < TOP {
            self.range <<= 8;
            self.shift_low();
            if self.range < TOP {
                self.range <<= 8;
                self.shift_low();
            }
        }
    }

    /// Whether the output reached `limit` and was cut short.
    pub fn is_full(&self) -> bool {
        self.full
    }

    /// Flush the final five bytes and return the code string, or `None` if
    /// it would exceed the limit.
    pub fn finish(mut self) -> Option<Vec<u8>> {
        for _ in 0..5 {
            self.shift_low();
        }
        (!self.full).then_some(self.out)
    }
}

/// The 7z range coder's decoder over an in-memory code string. Reading past
/// the end supplies zeros and is reported by [`RangeDecoder::finish`].
pub struct RangeDecoder<'a> {
    range: u32,
    code: u32,
    data: &'a [u8],
    pos: usize,
    overrun: bool,
}

impl<'a> RangeDecoder<'a> {
    /// Start decoding `data`; `None` if it does not begin like a code string
    /// (a zero byte, then a code value below 0xFFFFFFFF).
    pub fn new(data: &'a [u8]) -> Option<Self> {
        let mut d = RangeDecoder { range: u32::MAX, code: 0, data, pos: 0, overrun: false };
        if d.byte() != 0 {
            return None;
        }
        for _ in 0..4 {
            d.code = (d.code << 8) | u32::from(d.byte());
        }
        (d.code < u32::MAX).then_some(d)
    }

    fn byte(&mut self) -> u8 {
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

    fn threshold(&mut self, total: u32) -> u32 {
        self.range /= total;
        self.code / self.range
    }

    fn decode(&mut self, start: u32, size: u32) {
        self.code = self.code.wrapping_sub(start.wrapping_mul(self.range));
        self.range = self.range.wrapping_mul(size);
    }

    fn normalize_once(&mut self) {
        if self.range < TOP {
            self.code = (self.code << 8) | u32::from(self.byte());
            self.range <<= 8;
        }
    }

    fn normalize(&mut self) {
        if self.range < TOP {
            self.code = (self.code << 8) | u32::from(self.byte());
            self.range <<= 8;
            if self.range < TOP {
                self.code = (self.code << 8) | u32::from(self.byte());
                self.range <<= 8;
            }
        }
    }

    /// True if decoding ended exactly at the end of the code string, as the
    /// encoder left it.
    pub fn finish(&self) -> bool {
        self.code == 0 && !self.overrun && self.pos == self.data.len()
    }
}

/// Compress `data` with a fresh model, or `None` if the code string would
/// be longer than `limit` bytes.
pub fn encode_block(data: &[u8], limit: usize) -> Option<Vec<u8>> {
    let mut model = Ppmd::new(ORDER, arena_size(data.len()));
    let mut rc = RangeEncoder::new(limit);
    for &b in data {
        model.encode(&mut rc, b);
        if rc.is_full() {
            return None;
        }
    }
    rc.finish()
}

/// Decompress exactly `raw_len` bytes from `packed`, which must hold exactly
/// the code string.
pub fn decode_block(packed: &[u8], raw_len: usize) -> io::Result<Vec<u8>> {
    let corrupt = || io::Error::new(io::ErrorKind::InvalidData, "corrupt block (bad PPMd code string)");
    let mut model = Ppmd::new(ORDER, arena_size(raw_len));
    let mut rc = RangeDecoder::new(packed).ok_or_else(corrupt)?;
    let mut out = Vec::with_capacity(raw_len);
    for _ in 0..raw_len {
        out.push(model.decode(&mut rc).ok_or_else(corrupt)?);
    }
    if !rc.finish() {
        return Err(corrupt());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crc64::Crc64;

    fn encode_raw(data: &[u8], order: u32, size: u32) -> (Vec<u8>, u32) {
        let mut m = Ppmd::new(order, size);
        let mut rc = RangeEncoder::new(usize::MAX);
        for &b in data {
            m.encode(&mut rc, b);
        }
        (rc.finish().expect("no limit"), m.restarts())
    }

    fn decode_raw(packed: &[u8], len: usize, order: u32, size: u32) -> Option<Vec<u8>> {
        let mut m = Ppmd::new(order, size);
        let mut rc = RangeDecoder::new(packed)?;
        let out: Option<Vec<u8>> = (0..len).map(|_| m.decode(&mut rc)).collect();
        out.filter(|_| rc.finish())
    }

    fn lcg_bytes(n: usize, seed: u64) -> Vec<u8> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                (s >> 56) as u8
            })
            .collect()
    }

    /// Payload lengths, CRC-64s and restart counts of streams written by
    /// the unmodified 7-Zip 23.01 Ppmd7z encoder (order 6) for the same
    /// inputs.
    #[test]
    fn payloads_match_the_reference_encoder() {
        let text: Vec<u8> = b"Arithmetic coding: Glen G. Langdon. PPMd-H, order six.\n"
            .iter()
            .copied()
            .cycle()
            .take(128 << 10)
            .collect();
        let alphabet: Vec<u8> = (0..=255).collect();
        let cases: [(&[u8], u32, usize, u64, u32); 8] = [
            (&text, 2048, 115_595, 0xc74e_2ec8_9954_9a46, 2383),
            (&text, 65536, 120, 0x28f0_3b27_038d_0860, 0),
            (&text, 1 << 28, 120, 0x28f0_3b27_038d_0860, 0),
            (&alphabet, 2048, 307, 0x9b00_a6bc_a8de_a09a, 0),
            (&[0x00, 0xff].repeat(64 << 10), 2048, 56, 0x1afb_11cd_410e_600b, 0),
            (&[0u8; 64 << 10], 2048, 31, 0xfde6_c9db_79da_85c4, 0),
            (&[], 2048, 5, 0xcbe4_d2df_ee43_e035, 0),
            (&[0xff], 2048, 6, 0xecbd_f58e_466d_34c5, 0),
        ];
        for (data, size, len, crc, restarts) in cases {
            let (packed, r) = encode_raw(data, 6, size);
            assert_eq!(
                (packed.len(), Crc64::of(&packed), r),
                (len, crc, restarts),
                "{} bytes, arena {size}",
                data.len()
            );
            assert_eq!(decode_raw(&packed, data.len(), 6, size).as_deref(), Some(data));
        }
    }

    #[test]
    fn round_trips_across_orders_arenas_and_restarts() {
        let mut data = lcg_bytes(200_000, 7);
        data.extend(b"the quick brown fox ".repeat(5000));
        data.extend(lcg_bytes(50_000, 8).iter().map(|b| b % 7));
        for order in [2, 6, 16, 64] {
            for size in [2048, 1 << 16, 1 << 20] {
                let (packed, _) = encode_raw(&data, order, size);
                assert_eq!(
                    decode_raw(&packed, data.len(), order, size).as_deref(),
                    Some(&data[..]),
                    "o{order} m{size}"
                );
            }
        }
    }

    #[test]
    fn blocks_round_trip_and_incompressible_ones_are_refused() {
        let text = b"abracadabra, arithmetic coding ".repeat(4000);
        let packed = encode_block(&text, text.len() - 1).expect("compressible");
        assert_eq!(decode_block(&packed, text.len()).unwrap(), text);
        let noise = lcg_bytes(100_000, 3);
        assert!(encode_block(&noise, noise.len() - 1).is_none());
        let mut rc = RangeEncoder::new(70_000);
        let mut m = Ppmd::new(ORDER, arena_size(noise.len()));
        for &b in &noise {
            m.encode(&mut rc, b);
        }
        assert!(rc.is_full() && rc.out.capacity() <= 70_000, "capacity {}", rc.out.capacity());
        assert_eq!(decode_block(&encode_block(&[], usize::MAX).unwrap(), 0).unwrap(), b"");
    }

    /// A damaged code string must be rejected or decode to something; it
    /// must never panic (the block checksum catches the rest).
    #[test]
    fn damaged_code_strings_never_panic() {
        let data = [b"PPMd variant H ".repeat(2000), lcg_bytes(20_000, 11)].concat();
        let packed = encode_block(&data, usize::MAX).unwrap();
        let mut s = 1u64;
        for _ in 0..300 {
            let mut bad = packed.clone();
            for _ in 0..1 + (s % 4) {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1);
                let i = (s >> 33) as usize % bad.len();
                bad[i] ^= 1 << ((s >> 29) & 7);
            }
            let cut = if s % 5 == 0 { bad.len() / 2 } else { bad.len() };
            if let Ok(out) = decode_block(&bad[..cut], data.len()) {
                assert_eq!(out.len(), data.len());
            }
        }
        assert!(decode_block(&packed[..packed.len() - 1], data.len()).is_err());
        assert!(decode_block(&[packed.clone(), vec![0]].concat(), data.len()).is_err());
    }
}
