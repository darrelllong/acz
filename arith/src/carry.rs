// Copyright (c) 2026 Darrell D. E. Long. All rights reserved.
// Use and redistribution are subject to the BSD 3-clause license, with a
// dedication clause, in the LICENSE file at the root of this repository.
//
// Dedicated to the memory of my colleague Glen G. Langdon

//! Carry propagation for the byte-wise encoders: Langdon's carry-over problem.
//!
//! The code string is one long binary number, of which an encoder keeps the
//! low end in a register. Narrowing the interval adds to the register, and a
//! sum can carry out of it into bytes already shifted out. A carry adds one to
//! the byte before the register. A byte of `0xFF` becomes `0x00` and passes the
//! carry on, so a carry runs back through any `0xFF` bytes and stops at the
//! first byte below `0xFF`.
//!
//! The encoder therefore holds back that byte and counts the `0xFF` bytes
//! shifted out after it. Each byte that leaves the register is one of three
//! cases:
//!
//! - It arrives with a carry. The held byte is written plus one, the run is
//!   written as `0x00` bytes, and the new byte is held.
//! - It is `0xFF` with no carry. It joins the run.
//! - It is below `0xFF` with no carry. No later carry can pass it, so the held
//!   byte and the run are written as they are, and the new byte is held.
//!
//! One carry at most passes any position. Each interval lies within the one
//! before, whose width is less than one unit of the byte before the register.
//! Once a carry has gone into that byte the code value cannot reach the next
//! multiple of the unit, so a byte held after a carry is never incremented,
//! even if it is `0xFF`, and nothing before the held byte changes.

use std::io::{self, Write};

/// The bytes of a code string that a carry could still change.
pub(crate) struct Carry {
    held: u8,
    /// The `0xFF` bytes after `held`.
    run: u64,
}

impl Carry {
    /// Returns the state at the start of a code string: a held zero, which is
    /// the first byte written.
    pub(crate) const fn new() -> Self {
        Carry { held: 0, run: 0 }
    }

    /// Takes the next `byte` of the code string. `carry` says whether a carry
    /// came out of the register with it.
    #[inline]
    pub(crate) fn push<W: Write>(&mut self, out: &mut W, byte: u8, carry: bool) -> io::Result<()> {
        if carry {
            self.write(out, self.held.wrapping_add(1), 0x00)?;
        } else if byte == 0xFF {
            self.run += 1;
            return Ok(());
        } else {
            self.write(out, self.held, 0xFF)?;
        }
        self.held = byte;
        Ok(())
    }

    /// Writes the held byte and its run at the end of the code string.
    pub(crate) fn flush<W: Write>(&mut self, out: &mut W) -> io::Result<()> {
        self.write(out, self.held, 0xFF)
    }

    /// Writes `first` and then the run as copies of `fill`.
    #[inline]
    fn write<W: Write>(&mut self, out: &mut W, first: u8, fill: u8) -> io::Result<()> {
        out.write_all(&[first])?;
        while self.run > 0 {
            out.write_all(&[fill])?;
            self.run -= 1;
        }
        Ok(())
    }
}
