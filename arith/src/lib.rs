// Copyright (c) 2026 Darrell D. E. Long. All rights reserved.
// Use and redistribution are subject to the BSD 3-clause license, with a
// dedication clause, in the LICENSE file at the root of this repository.
//
// Dedicated to the memory of my colleague Glen G. Langdon

//! Arithmetic coding in safe Rust.
//!
//! A finite-precision arithmetic coder of the kind described by Langdon (1984)
//! and made practical by Witten, Neal and Cleary (1987) and by Moffat, Neal and
//! Witten (1998).
//!
//! Following Langdon, the crate separates the coder from the model. The coder
//! ([`Encoder`], [`Decoder`]) knows nothing about the data. For each event it
//! is given a sub-interval `[cum_low, cum_high)` of `[0, total)` and narrows
//! its current interval to match (Langdon's double recursion, equations 7 and
//! 8). The model ([`Model`], [`FrequencyTable`], [`BitModel`]) supplies those
//! sub-intervals and may adapt as it goes.
//!
//! # Precision, carry and termination
//!
//! Langdon names three problems with the ideal code.
//!
//! Precision. The product of the interval width and a probability grows without
//! bound. Here the width is kept in a 32-bit register and products are
//! truncated. Sub-intervals are computed from cumulative counts, so they leave
//! no gaps and do not overlap.
//!
//! Carry-over. Adding to the low end can carry into bits already written.
//! Langdon describes bit stuffing. This coder counts pending bits instead,
//! after Witten, Neal and Cleary: while the interval straddles the midpoint
//! inside the middle half, the next bit is not known, so it is counted, and
//! written inverted once the carry is resolved. No bit changes after it is
//! written.
//!
//! Termination. [`Encoder::finish`] writes enough bits that a decoder driven
//! through the same events reads the bits written and no more. Given a way to
//! know where the message ends, code strings can be embedded in containers or
//! concatenated.
//!
//! # Three coders
//!
//! * [`Encoder`] and [`Decoder`] renormalize a bit at a time, take intervals or
//!   binary events, and read from any [`std::io::Read`].
//!
//! * [`MultiRangeEncoder`] and [`MultiRangeDecoder`] take intervals with totals
//!   up to 2²⁰ and binary events, divide once per interval, and renormalize a
//!   byte at a time. They decode from a slice.
//!
//! * [`RangeEncoder`] and [`RangeDecoder`] take binary events only and
//!   renormalize a byte at a time. They decode from a slice.
//!
//! # References
//!
//! G. G. Langdon, Jr., *An Introduction to Arithmetic Coding*, IBM J. Res.
//! Develop. 28(2), 1984, pp. 135–149.
//!
//! I. H. Witten, R. M. Neal and J. G. Cleary, *Arithmetic Coding for Data
//! Compression*, Commun. ACM 30(6), 1987, pp. 520–540.
//!
//! A. Moffat, R. M. Neal and I. H. Witten, *Arithmetic Coding Revisited*, ACM
//! Trans. Inf. Syst. 16(3), 1998, pp. 256–294.
//!
//! # Example
//!
//! ```
//! use arith::{Encoder, Decoder, FrequencyTable};
//!
//! let message = b"abracadabra";
//!
//! // An adaptive order-0 model over the 256 byte values.
//! let mut model = FrequencyTable::new(256);
//! let mut enc = Encoder::new(Vec::new());
//! for &b in message {
//!     enc.encode_symbol(&mut model, b as usize).unwrap();
//! }
//! let code = enc.finish().unwrap();
//!
//! // The decoder starts from the same model.
//! let mut model = FrequencyTable::new(256);
//! let mut dec = Decoder::new(&code[..]).unwrap();
//! let decoded: Vec<u8> = (0..message.len())
//!     .map(|_| dec.decode_symbol(&mut model).unwrap() as u8)
//!     .collect();
//! assert_eq!(decoded, message);
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod carry;
mod coder;
mod model;
mod multirange;
mod range;

pub use coder::{Decoder, Encoder, MAX_TOTAL};
pub use model::{BitModel, FrequencyTable, Model, BIT_PRECISION};
pub use multirange::{MultiRangeDecoder, MultiRangeEncoder, MULTI_MAX_TOTAL};
pub use range::{RangeDecoder, RangeEncoder};
