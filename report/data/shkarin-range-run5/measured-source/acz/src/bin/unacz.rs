// Copyright (c) 2026 Darrell D. E. Long. All rights reserved.
// Use and redistribution are subject to the BSD 3-clause license, with a
// dedication clause, in the LICENSE file at the root of this repository.
//
// Dedicated to the memory of my colleague Glen G. Langdon

#![forbid(unsafe_code)]

fn main() -> std::process::ExitCode {
    acz::cli::main(acz::cli::Mode::Decompress, false)
}
