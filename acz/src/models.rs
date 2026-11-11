// Copyright (c) 2026 Darrell D. E. Long. All rights reserved.
// Use and redistribution are subject to the BSD 3-clause license, with a
// dedication clause, in the LICENSE file at the root of this repository.
//
// Dedicated to the memory of my colleague Glen G. Langdon

//! The byte models of the `acz` format.
//!
//! A model codes one block, whose length is in the block framing (see
//! [`crate::format`]), so no end-of-stream event is needed. Every block starts
//! from a new model, which lets blocks be coded in parallel.

use std::io;

use arith::{
    BitModel, Decoder, Encoder, FrequencyTable, MultiRangeDecoder, MultiRangeEncoder, RangeDecoder, RangeEncoder,
};

use crate::cm::Cm;

/// The model. An order is the number of preceding bytes used as context.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Order {
    /// An adaptive frequency table over the 256 byte values.
    Zero,
    /// One adaptive frequency table for each previous byte (Langdon §1, the
    /// dependent model).
    One,
    /// The previous two bytes as context, with each byte coded as eight binary
    /// events along a binary tree, each with an adaptive skew (Langdon §4, the
    /// linearized binary tree).
    Two,
    /// Prediction by partial matching, up to order 6 (see [`crate::ppm`]).
    Ppm,
    /// Context mixing of orders 0–4 and 6, a word model, record contexts and
    /// a match model (see [`crate::cm`]).
    Mix,
}

/// Returns an upper bound, in bytes, on the memory a model of `order` needs for
/// a block of `block_size` bytes, beyond the input and output buffers. It is
/// the same for encoding and decoding.
pub fn model_memory(order: Order, block_size: usize) -> u64 {
    match order {
        Order::Zero => 1 << 12,
        Order::One => 256 * (1 << 12),
        Order::Two => 2 << 24,
        Order::Mix => Cm::memory(block_size),
        Order::Ppm => crate::ppm::Ppm::memory(block_size),
    }
}

impl Order {
    /// Returns a short name for messages.
    pub fn name(self) -> &'static str {
        match self {
            Order::Zero => "order-0",
            Order::One => "order-1",
            Order::Two => "order-2",
            Order::Mix => "context mixing",
            Order::Ppm => "PPM",
        }
    }

    /// Returns the tag stored in the stream header.
    pub fn tag(self) -> u8 {
        match self {
            Order::Zero => 0,
            Order::One => 1,
            Order::Two => 2,
            Order::Ppm => 3,
            Order::Mix => 4,
        }
    }

    /// Returns the model with tag `tag`, if there is one.
    pub fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            0 => Some(Order::Zero),
            1 => Some(Order::One),
            2 => Some(Order::Two),
            3 => Some(Order::Ppm),
            4 => Some(Order::Mix),
            _ => None,
        }
    }
}

/// A model and its running context.
enum ByteModel {
    Zero(FrequencyTable),
    One { tables: Vec<FrequencyTable>, prev: usize },
    Two { nodes: Vec<BitModel<4>>, ctx: usize },
    Mix(Box<Cm>),
    Ppm(Box<crate::ppm::Ppm>),
}

impl ByteModel {
    fn new(order: Order, len: usize) -> Self {
        match order {
            Order::Zero => ByteModel::Zero(FrequencyTable::new(256)),
            Order::One => ByteModel::One { tables: vec![FrequencyTable::new(256); 256], prev: 0 },
            Order::Two => ByteModel::Two { nodes: vec![BitModel::new(); 1 << 24], ctx: 0 },
            Order::Mix => ByteModel::Mix(Box::new(Cm::new(len))),
            Order::Ppm => ByteModel::Ppm(Box::new(crate::ppm::Ppm::new(len))),
        }
    }
}

/// A `Write` sink that fails once it would pass `limit` bytes. It never asks
/// for capacity beyond `limit`, though the allocator may round a request up.
pub(crate) struct Bounded {
    buf: Vec<u8>,
    limit: usize,
}

impl Bounded {
    pub(crate) fn new(limit: usize) -> Self {
        Bounded { buf: Vec::new(), limit }
    }

    pub(crate) fn into_inner(self) -> Vec<u8> {
        self.buf
    }
}

impl io::Write for Bounded {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let need = self.buf.len() + data.len();
        if need > self.limit {
            return Err(io::Error::new(io::ErrorKind::WriteZero, "code string would not be smaller"));
        }
        reserve_bounded(&mut self.buf, need, self.limit);
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Makes room in `v` for `need` bytes. The capacity at least doubles, from 64
/// KiB, but a request never passes `limit`.
pub(crate) fn reserve_bounded(v: &mut Vec<u8>, need: usize, limit: usize) {
    if need <= v.capacity() {
        return;
    }
    let target = v.capacity().saturating_mul(2).max(1 << 16).max(need).min(limit.max(need));
    v.reserve_exact(target - v.len());
}

/// Compresses one block with a new model of the given order, or returns `None`
/// if the code string would not be smaller than the block, which the caller
/// then stores.
///
/// Orders 0 and 1 give each byte an interval of a frequency total and use the
/// bit-wise coder ([`Encoder`]). PPM codes several events for a byte, intervals
/// and binary decisions, and uses the multi-symbol range coder
/// ([`MultiRangeEncoder`]). Order 2 and context mixing predict a bit at a time
/// and use the binary range coder ([`RangeEncoder`]), which costs least per
/// decision.
///
/// The output never asks for more than `data.len() - 1` bytes, even for
/// incompressible data.
pub fn encode_block(order: Order, data: &[u8]) -> Option<Vec<u8>> {
    let limit = data.len().checked_sub(1)?;
    match order {
        Order::Zero | Order::One => encode_symbols(order, data, limit),
        Order::Ppm => encode_ppm(data, limit),
        Order::Two | Order::Mix => encode_binary(order, data, limit),
    }
}

/// Codes a block at order 0 or 1 on the bit-wise coder.
fn encode_symbols(order: Order, data: &[u8], limit: usize) -> Option<Vec<u8>> {
    let mut enc = Encoder::new(Bounded::new(limit));
    let r: io::Result<()> = (|| {
        match ByteModel::new(order, data.len()) {
            ByteModel::Zero(mut t) => {
                for &b in data {
                    enc.encode_symbol(&mut t, usize::from(b))?;
                }
            }
            ByteModel::One { mut tables, mut prev } => {
                for &b in data {
                    enc.encode_symbol(&mut tables[prev], usize::from(b))?;
                    prev = usize::from(b);
                }
            }
            _ => unreachable!("PPM and the binary models have their own coders"),
        }
        Ok(())
    })();
    // The only possible error is the bounded sink filling up.
    r.ok()?;
    enc.finish().ok().map(Bounded::into_inner)
}

/// Codes a block with PPM on the multi-symbol range coder.
fn encode_ppm(data: &[u8], limit: usize) -> Option<Vec<u8>> {
    let ByteModel::Ppm(mut m) = ByteModel::new(Order::Ppm, data.len()) else { unreachable!("a PPM model") };
    let mut enc = MultiRangeEncoder::new(Bounded::new(limit));
    // The only possible error is the bounded sink filling up.
    data.iter().try_for_each(|&b| m.encode(&mut enc, b)).ok()?;
    enc.finish().ok().map(Bounded::into_inner)
}

/// Codes a block at order 2 or with context mixing on the binary range coder.
fn encode_binary(order: Order, data: &[u8], limit: usize) -> Option<Vec<u8>> {
    let mut enc = RangeEncoder::new(Bounded::new(limit));
    let r: io::Result<()> = (|| {
        match ByteModel::new(order, data.len()) {
            // Each context (the previous two bytes) owns a binary tree of
            // 255 decisions. The node index starts at 1 and appends each
            // coded bit, so it names the bits seen so far within the byte;
            // after eight bits it is 256 + the byte. That is 2^16 contexts
            // of 256 slots: 2^24 bit models.
            ByteModel::Two { mut nodes, mut ctx } => {
                for &byte in data {
                    let base = ctx << 8;
                    let mut node = 1usize;
                    for i in (0..8).rev() {
                        let bit = (byte >> i) & 1 != 0;
                        enc.encode_bit(&mut nodes[base + node], bit)?;
                        node = (node << 1) | usize::from(bit);
                    }
                    ctx = ((ctx << 8) | usize::from(byte)) & 0xFFFF;
                }
            }
            ByteModel::Mix(mut cm) => {
                for &byte in data {
                    for i in (0..8).rev() {
                        let bit = (byte >> i) & 1 != 0;
                        enc.encode_bit_p(bit, cm.p1())?;
                        cm.update(bit);
                    }
                }
            }
            _ => unreachable!("multi-symbol models use encode_symbols"),
        }
        Ok(())
    })();
    r.ok()?;
    enc.finish().ok().map(Bounded::into_inner)
}

/// Decompresses one block of `raw_len` bytes from `packed`, which must be the
/// whole code string and nothing more.
pub fn decode_block(order: Order, packed: &[u8], raw_len: usize) -> io::Result<Vec<u8>> {
    match order {
        Order::Zero | Order::One => decode_symbols(order, packed, raw_len),
        Order::Ppm => decode_ppm(packed, raw_len),
        Order::Two | Order::Mix => decode_binary(order, packed, raw_len),
    }
}

fn decode_symbols(order: Order, packed: &[u8], raw_len: usize) -> io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(raw_len);
    let mut dec = Decoder::new(packed)?;
    match ByteModel::new(order, raw_len) {
        ByteModel::Zero(mut t) => {
            for _ in 0..raw_len {
                out.push(dec.decode_symbol(&mut t)? as u8);
            }
        }
        ByteModel::One { mut tables, mut prev } => {
            for _ in 0..raw_len {
                let s = dec.decode_symbol(&mut tables[prev])?;
                out.push(s as u8);
                prev = s;
            }
        }
        _ => unreachable!("PPM and the binary models have their own coders"),
    }
    // Any code value in the final interval decodes to the same bytes. Only
    // the termination the encoder writes is accepted, with no bytes left
    // over, so a payload changed after its last significant bit, or with
    // bytes appended, is rejected.
    if !dec.is_canonical_end() || !dec.finish().is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "corrupt block (bad code string termination)"));
    }
    Ok(out)
}

fn decode_ppm(packed: &[u8], raw_len: usize) -> io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(raw_len);
    let mut dec = MultiRangeDecoder::new(packed)?;
    let ByteModel::Ppm(mut m) = ByteModel::new(Order::Ppm, raw_len) else { unreachable!("a PPM model") };
    for _ in 0..raw_len {
        out.push(m.decode(&mut dec)?);
    }
    // As above: only the code string the encoder writes is accepted.
    if !dec.is_canonical_end() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "corrupt block (bad code string termination)"));
    }
    Ok(out)
}

fn decode_binary(order: Order, packed: &[u8], raw_len: usize) -> io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(raw_len);
    let mut dec = RangeDecoder::new(packed)?;
    match ByteModel::new(order, raw_len) {
        ByteModel::Two { mut nodes, mut ctx } => {
            for _ in 0..raw_len {
                let base = ctx << 8;
                let mut node = 1usize;
                while node < 256 {
                    let bit = dec.decode_bit(&mut nodes[base + node]);
                    node = (node << 1) | usize::from(bit);
                }
                let byte = node - 256;
                out.push(byte as u8);
                ctx = ((ctx << 8) | byte) & 0xFFFF;
            }
        }
        ByteModel::Mix(mut cm) => {
            for _ in 0..raw_len {
                let mut byte = 0u8;
                for _ in 0..8 {
                    let bit = dec.decode_bit_p(cm.p1());
                    cm.update(bit);
                    byte = (byte << 1) | u8::from(bit);
                }
                out.push(byte);
            }
        }
        _ => unreachable!("multi-symbol models use decode_symbols"),
    }
    dec.finish().map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("corrupt block: {e}")))?;
    Ok(out)
}
