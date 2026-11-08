# arith and acz

<p align="center">
  <img src="docs/images/glen-langdon.png" width="280" alt="Glen G. Langdon, from his IBM badge photograph"><br>
  <em>Dedicated to the memory of my colleague Glen G. Langdon</em>
</p>

`arith` is a finite-precision arithmetic-coding library in Rust, based on
G. G. Langdon, Jr., *An Introduction to Arithmetic Coding* (IBM J. Res.
Develop. 28(2), 1984). `acz` is a command-line file compressor built on
the library. Neither uses external crate dependencies or `unsafe` code.

## Building

```sh
cargo build --release    # target/release/acz, unacz, aczcat
cargo test --release
```

## Using acz

```sh
acz report.txt              # writes report.txt.acz; report.txt is kept
unacz report.txt.acz        # writes report.txt (same as acz -d)
aczcat report.txt.acz       # writes to standard output (same as acz -dc)
acz -t report.txt.acz       # checks the file without writing anything
acz --rm report.txt         # removes report.txt once report.txt.acz is written

tar cf - dir | acz > dir.tar.acz     # no file names: read stdin, write stdout
acz -c a b > ab.acz                  # several inputs into one stream
cat a.acz b.acz | unacz              # concatenated streams decode in order
```

The defaults are model `-4`, all available cores, and `--memory=auto`.
The automatic budget is half the detected physical memory or cgroup v2
memory limit, whichever is smaller; it is 4 GiB if neither can be detected.
Decompression reads the model and block size from the stream.

### Options

| option | meaning |
|---|---|
| `-z`, `--compress` | compress (the default for `acz`) |
| `-d`, `--decompress`, `--uncompress` | decompress (the default for `unacz` and `aczcat`) |
| `-t`, `--test` | decompress and check, without writing output |
| `-c`, `--stdout`, `--to-stdout` | write to standard output (the default for `aczcat`) |
| `-o FILE`, `--output FILE` | write everything to one file |
| `-f`, `--force` | replace existing files; allow compressed data to or from a terminal |
| `-k`, `--keep` | keep input files (the default) |
| `--rm` | remove each input after its output file is installed; ignored with `-c`, `-o`, or `-t` |
| `-0` … `-4`, `--order N` | select a model; default `-4` (see below) |
| `-T N`, `--threads N` | at most N worker threads (default: all cores) |
| `--block SIZE` | block size, 64K to 1G (default 16M) |
| `--dedup[=SIZE]` | remove long repeats up to SIZE back (default 256M); off by default |
| `--no-dedup` | disable repeat removal; the last `--dedup` or `--no-dedup` option wins |
| `--memory SIZE` | budget for models and buffers, or `auto` (the default, described above) |
| `-v`, `--verbose` | report sizes on standard error |
| `-q`, `--quiet` | suppress warnings |
| `-h`, `--help`; `-V`, `--version` | show help or version |

A long option's value can follow `=` or be the next argument
(`--block=1M` or `--block 1M`), except for `--dedup`, whose value is
optional and must follow `=`. The repeat window must be larger than the
block. A requested window larger than the block remains larger after
rounding. Sizes are byte counts with optional `K`, `M` or `G` suffixes
(powers of 1024). `16M`, `16MB` and `16MiB` mean the same size; suffixes
are case-insensitive. Block and window sizes are rounded up to powers of
two. Short flags combine: `-dc` is `-d -c`. Run `acz --help` for all options.

The exit status is 0 on success and 1 if any input failed (the others are
still processed). If the reader of standard output closes early, as in
`aczcat big.acz | head`, acz stops quietly with status 141.

### File handling and resource limits

* acz does not modify input files. It writes each output file to a temporary
  file and installs it after compression or decompression succeeds.
  `--rm` removes the input only after its output file has been installed.
* Existing files are not replaced without `-f`, including files created
  while acz runs. acz rejects an output that refers to an input file.
* An output file specified with `-o` is installed only if all inputs succeed.
* acz verifies CRC-64 checksums of the header, each block, and the decoded
  data. A checksum mismatch is an error. Bytes already written to standard
  output cannot be withdrawn.
* `--memory` sets the budget for models, block buffers, and the repeat
  window when enabled. acz uses their estimated allocation sizes to limit
  the worker count. A stream that cannot fit with one worker is rejected
  before its model and history buffers are allocated. The budget does not
  change the encoded bytes and is not a hard limit on total process memory.
* The same input and encoding settings produce identical bytes across
  platforms and thread counts.
* Maximum expansion is 17 bytes per block (29 with `--dedup`), plus
  33 bytes per input file.

### Choosing a model

| model | predicts each byte from |
|---|---|
| `-4` (default) | a weighted combination of context-model predictions (context mixing) |
| `-3` | contexts of up to 6 preceding bytes, with escape to shorter contexts (PPMd variant H) |
| `-2` | the previous two bytes, one bit at a time |
| `-1` | the previous byte |
| `-0` | byte frequencies alone |

In release 0.5.0, level 3 is a safe-Rust translation of the PPMd-H model,
memory allocator, and range coder in 7-Zip's `Ppmd7` implementation by
Igor Pavlov, based on Dmitry Shkarin's PPMd. Its algorithms and data
structures follow that implementation. The source credits both authors.

At the default 16 MiB block size, memory planning budgets about 213 MiB
per `-4` worker, so a 4 GiB budget admits 19 workers. For `-3` with multiple
workers, it budgets about 320 MiB per worker (12 workers in 4 GiB),
including queued block buffers.
Its model uses an arena of 16 bytes per modeled input byte, clamped to
1–256 MiB, plus its estimator tables and fixed state. With `--dedup`,
the modeled input is the residual after copies are removed.

### Long-repeat removal (`--dedup`)

`--dedup` finds repeats of 256 bytes or more anywhere in the last 256 MiB
of input (or `--dedup=SIZE`) and replaces them with copy commands before
model encoding. Repeats can cross block boundaries. For example, to
enable repeat removal when compressing an archive:

```sh
tar cf - backups/ | acz --dedup > backups.tar.acz
```

Repeat removal is off by default and can be enabled with any model.
It does not always improve compression. At the default window it adds
about 320 MiB to the compression memory budget and 290 MiB to the
decompression memory budget.

### Record contexts

Context mixing detects fixed-length records and predicts each byte from
the corresponding positions in preceding records. These contexts are
enabled in `-4` and require no additional option.

## Measurements

Benchmark results are being revised. The technical report will present
reviewed measurements with their exact implementation and format versions,
input hashes, build identities, settings, scripts, and raw results.

## Using the library

```rust
use arith::{Decoder, Encoder, FrequencyTable};

let mut model = FrequencyTable::new(256);
let mut enc = Encoder::new(Vec::new());
for &b in b"abracadabra" {
    enc.encode_symbol(&mut model, usize::from(b))?;
}
let code = enc.finish()?;

let mut model = FrequencyTable::new(256);
let mut dec = Decoder::new(&code[..])?;
let first = dec.decode_symbol(&mut model)?; // b'a'
```

* `Encoder` and `Decoder` encode and decode symbols using cumulative
  frequency intervals supplied by a model. The frequency total must be
  positive and at most 2³⁰. The coder uses bit normalization and buffers underflow bits
  until their values are determined.
* `RangeEncoder` and `RangeDecoder` encode and decode binary decisions
  using byte normalization.
* `FrequencyTable` maintains adaptive symbol frequencies. `BitModel`
  maintains an adaptive probability for a binary decision. Custom models
  implement the `Model` trait.

The decoder needs a symbol count, an explicit end symbol, or enclosing
file framing to determine when to stop.

## The .acz format

For each input file, acz writes a 16-byte header containing the model,
block size, repeat-window size, and header checksum. The data blocks
follow, then an end marker, data checksum, and original length.

Each block's model is initialized separately, allowing model encoding and
decoding in parallel. If a block's compressed representation is not smaller
than its input, acz stores the input bytes with a block header and checksum.
With `--dedup`, the model encodes the residual bytes after repeat removal;
literal and copy commands specify how to reconstruct the original input.
Repeat removal and reconstruction run in stream order.

The full specification is the documentation of
[`acz/src/format.rs`](acz/src/format.rs).

The current format is version 11. Format changes increment the version;
the decoder rejects unsupported versions. Format compatibility is not
guaranteed before release 1.0.

## License

BSD 3-clause, with one added condition: the dedication to Glen G. Langdon
must be kept in all copies and derivative works. See [LICENSE](LICENSE).
