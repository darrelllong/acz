// Copyright (c) 2026 Darrell D. E. Long. All rights reserved.
// Use and redistribution are subject to the BSD 3-clause license, with a
// dedication clause, in the LICENSE file at the root of this repository.
//
// Dedicated to the memory of my colleague Glen G. Langdon

//! `acz`: a streaming file compressor built on the [`arith`] arithmetic
//! coder.
//!
//! * [`format`](mod@format) defines the `.acz` container: a CRC-64-checked
//!   header, independently coded blocks each with a CRC-64, and a CRC-64 +
//!   length trailer. [`format::compress`] and [`format::decompress`] work
//!   on any `Read`/`Write` pair. Worker admission accounts for model and
//!   block-buffer storage; the budget is not a limit on total process memory.
//! * [`models`] holds the byte models (orders 0, 1, 2, PPM and context
//!   mixing).
//! * [`ppm`] is prediction by partial matching, the model behind model 3.
//! * [`cm`] is the context-mixing predictor behind model 4.
//! * [`dedup`] is the optional long-repeat removal ahead of the block coders.
//! * [`crc64`] computes CRC-64/XZ.
//! * [`cli`] is the command-line front end shared by the binaries.

#![forbid(unsafe_code)]

pub mod cli;
pub mod cm;
pub mod crc64;
pub mod dedup;
pub mod format;
pub mod models;
pub mod ppm;

pub use format::{compress, decompress, DecodeOptions, Options, Stats};
pub use models::Order;
