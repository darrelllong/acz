// Copyright (c) 2026 Darrell D. E. Long. All rights reserved.
// Use and redistribution are subject to the BSD 3-clause license, with a
// dedication clause, in the LICENSE file at the root of this repository.
//
// Dedicated to the memory of my colleague Glen G. Langdon

//! # arith — arithmetic coding in pure safe Rust
//!
//! This crate implements a finite-precision arithmetic coder of the kind
//! described in G. G. Langdon, Jr., *"An Introduction to Arithmetic Coding"*,
//! IBM J. Res. Develop. 28(2), 1984, and made practical by Witten, Neal &
//! Cleary, *"Arithmetic Coding for Data Compression"*, CACM 30(6), 1987, and
//! Moffat, Neal & Witten, *"Arithmetic Coding Revisited"*, ACM TOIS 16(3), 1998.
//!
//! Following Langdon, the crate separates the two halves of a compression
//! system:
//!
//! * the **coder** ([`Encoder`] / [`Decoder`]) knows nothing about the data.
//!   It is handed, for each event, a sub-interval `[cum_low, cum_high)` of
//!   `[0, total)` and narrows its current interval `[low, high]`
//!   accordingly (Langdon's "double recursion", Eqs. 7–8).
//! * the **model** ([`Model`], [`FrequencyTable`], [`BitModel`]) supplies those
//!   sub-intervals — the relative frequencies — and may adapt as it goes.
//!
//! ## Finite precision, carry-over and termination
//!
//! Langdon identifies three engineering problems with the ideal (Elias) code:
//!
//! 1. **Precision** — the product `A(s) × p` grows without bound. As in
//!    Pasco / Martin / Jones, the width is kept in a fixed 32-bit register and
//!    the products are truncated. Sub-intervals are computed from cumulative
//!    counts so that `C(s,k+1) = C(s,k) + A(s,k)`: there are no gaps and no
//!    overlaps (decodability criterion, Eq. 11, holds with equality).
//! 2. **Carry-over** — adding to `C` may ripple a carry into bits already
//!    emitted. Langdon describes *bit-stuffing*; this crate uses the
//!    *pending-bit* ("underflow") technique of
//!    Witten–Neal–Cleary: when the interval straddles the midpoint inside the
//!    middle half, the next output bit is not yet known, so we count it and
//!    emit it (inverted) once the carry is resolved. No bit is ever revised
//!    after it is written, so the output is a pure stream.
//! 3. **Termination** — [`Encoder::finish`] writes exactly enough bits that
//!    a decoder driven through the same events consumes *precisely* the
//!    bits the encoder produced (no read-ahead past the end). Given a way to
//!    know when the message ends (a length, an end symbol, or framing), code
//!    strings can therefore be embedded in containers or concatenated.
//!
//! ## Three coders
//!
//! * [`Encoder`] / [`Decoder`] — bit-at-a-time renormalization, any
//!   alphabet (multi-symbol intervals or binary events), streaming
//!   [`std::io::Read`] input.
//! * [`MultiRangeEncoder`] / [`MultiRangeDecoder`] — multi-symbol intervals
//!   (totals up to 2²⁰) and binary events, one division per interval and
//!   byte-at-a-time renormalization with carry propagation (Moffat, Neal
//!   and Witten, 1998), decoding from a slice.
//! * [`RangeEncoder`] / [`RangeDecoder`] — binary events only, byte-at-a-time
//!   renormalization with carry propagation, decoding from a slice.
//!
//! ## Example
//!
//! ```
//! use arith::{Encoder, Decoder, FrequencyTable};
//!
//! let message = b"abracadabra";
//!
//! // Encode with an adaptive order-0 model over 256 byte values.
//! let mut model = FrequencyTable::new(256);
//! let mut enc = Encoder::new(Vec::new());
//! for &b in message {
//!     enc.encode_symbol(&mut model, b as usize).unwrap();
//! }
//! let code = enc.finish().unwrap();
//!
//! // Decode with an identically initialized model.
//! let mut model = FrequencyTable::new(256);
//! let mut dec = Decoder::new(&code[..]).unwrap();
//! let decoded: Vec<u8> = (0..message.len())
//!     .map(|_| dec.decode_symbol(&mut model).unwrap() as u8)
//!     .collect();
//! assert_eq!(decoded, message);
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod coder;
mod model;
mod multirange;
mod range;

pub use coder::{Decoder, Encoder, MAX_TOTAL};
pub use model::{BitModel, FrequencyTable, Model, BIT_PRECISION};
pub use multirange::{MultiRangeDecoder, MultiRangeEncoder, MULTI_MAX_TOTAL};
pub use range::{RangeDecoder, RangeEncoder};
