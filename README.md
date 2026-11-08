# arith and acz

<p align="center">
  <img src="docs/images/glen-langdon.png" width="280" alt="Glen G. Langdon, from his IBM badge photograph"><br>
  <em>Dedicated to the memory of my colleague Glen G. Langdon</em>
</p>

`arith` is an arithmetic-coding library in pure safe Rust, after G. G.
Langdon, Jr., *An Introduction to Arithmetic Coding* (IBM J. Res. Develop.
28(2), 1984). `acz` is a file compressor built on it, used like `gzip` or
`zstd`. The library and the compressor use no other packages and no
`unsafe` code, and they run on any processor Rust supports.

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

tar cf - dir | acz > dir.tar.acz     # with no file names, acz is a filter
acz -c a b > ab.acz                  # several inputs into one stream
cat a.acz b.acz | unacz              # concatenated streams decode in order
```

No option is needed for good results: the defaults are the strongest
model, all available cores, and a memory budget of half the machine's
physical memory (`--memory=auto`). Decompression needs no options to
choose a model or size, because the stream records them.

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
| `--rm` | remove each input file after its output is safely written |
| `-0` … `-4`, `--order N` | model; higher compresses better, and `-4`, the default, best (see below) |
| `-T N`, `--threads N` | at most N worker threads (default: all cores) |
| `--block SIZE` | block size, 64K to 1G (default 16M) |
| `--dedup[=SIZE]` | remove long repeats up to SIZE back (default 256M); off by default |
| `--no-dedup` | disable repeat removal; the last `--dedup` or `--no-dedup` option wins |
| `--memory SIZE` | memory budget, or `auto` (the default): half of physical memory, or 4G if that cannot be found |
| `-v`, `--verbose` | report sizes on standard error |
| `-q`, `--quiet` | suppress warnings |
| `-h`, `--help`; `-V`, `--version` | show help or version |

A long option's value can follow `=` or be the next argument
(`--block=1M` or `--block 1M`), except for `--dedup`, whose value is
optional and must follow `=`. The window must be larger than the block; a
window given larger than the block stays larger after rounding. Sizes are byte counts with an optional `K`,
`M` or `G` (powers of 1024; `16M`, `16MB` and `16MiB` are the same, in
either case). Block and window sizes are rounded up to a power of two.
Short flags combine: `-dc` is `-d -c`. `acz --help` gives the full text.

The exit status is 0 on success and 1 if any input failed (the others are
still processed). If the reader of standard output goes away, as in
`aczcat big.acz | head`, acz stops quietly with status 141.

### What acz guarantees

* **Inputs are never lost.** acz never modifies an input. Each output is
  written to a temporary file and moved into place only after compression
  or decompression has succeeded, and `--rm` removes an input only after
  that.
* **Existing files are not replaced** without `-f`, even one created while
  acz runs. An output that is the same file as an input, under any name,
  is refused.
* **`-o` is all or nothing:** if any input fails, the output is not written.
* **Corruption is caught.** acz checks a CRC-64 of the header, of every
  block and of all the decoded data, and refuses the output when a check
  fails (output already sent to a pipe cannot be taken back).
* **Memory follows `--memory`.** acz works out what each worker needs
  (its model and block buffers), runs only as many workers as fit in the
  budget, and refuses a stream it cannot fit before allocating anything.
  The budget never changes the output. The budget is not a hard limit on
  the whole process.
* **Output is the same everywhere:** the same input and options give the
  same bytes on every platform and with any number of threads.
* **Incompressible data barely grows:** by at most 17 bytes per block
  (29 with `--dedup`) plus 33 bytes per input file.

### Choosing a model

| model | predicts each byte from |
|---|---|
| `-4` (default) | many contexts, mixed (context mixing, the PAQ family) |
| `-3` | the longest context that has seen it, up to 6 bytes (PPMd variant H) |
| `-2` | the previous two bytes, one bit at a time |
| `-1` | the previous byte |
| `-0` | byte frequencies alone |

In release 0.5.0, level 3 is a safe-Rust translation of the PPMd-H model,
memory allocator, and range coder in 7-Zip's `Ppmd7` implementation by
Igor Pavlov, based on Dmitry Shkarin's PPMd. Its algorithms and data
structures follow that implementation. The source credits both authors.

Blocks can be coded independently by multiple workers; actual scaling
depends on the input size, memory budget, and machine.

At the default 16 MiB block size, each `-4` worker is allowed about 213
MiB, so a 4 GiB budget admits 19 workers (a machine with 8 GiB of memory
has that budget by default). With multiple workers, `-3` is allowed about
320 MiB per worker (12 workers in 4 GiB), including queued block buffers.
Its model uses an arena of 16 bytes per modeled input byte, clamped to
1–256 MiB, plus its estimator tables and fixed state. With `--dedup`,
the modeled input is the residual after copies are removed. The other
models need about 70 to 100 MB per worker.

### Long-repeat removal (`--dedup`)

`--dedup` finds repeats of 256 bytes or more anywhere in the last 256 MiB
of input (or `--dedup=SIZE`) and replaces them with references before the
model sees the data. It helps when the same long content recurs far apart,
beyond one block, as in an archive of similar files:

```sh
tar cf - backups/ | acz --dedup > backups.tar.acz
```

Its effect depends on the input and the downstream model; it is off by
default. At the default window it needs about 320 MiB more memory
to compress and 290 MiB more to decompress. It works with every model.

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

* `Encoder` and `Decoder` code any alphabet: a model gives each symbol an
  interval of a total of at most 2³⁰. The carry problem Langdon describes
  is solved with pending bits, so no output bit is ever revised, and the
  decoder reads exactly the bits the encoder wrote.
* `RangeEncoder` and `RangeDecoder` code binary decisions a byte at a time,
  for models that predict bits.
* `FrequencyTable` is an adaptive model for any alphabet; `BitModel` is an
  adaptive probability for one binary decision. Implement the `Model`
  trait for your own.

A code string does not mark its own end: the decoder must know how many
symbols to read, from a length, an end symbol, or framing.

## The .acz format

For each input file, acz writes a 16-byte header (which model, block size
and repeat window were used, and a checksum), then the data in blocks of
the block size, then an end marker and a final checksum and length.

Each block is compressed on its own, starting from an untrained model;
that is what lets several cores work on different blocks at once. If
compressing a block would not make it smaller, acz writes the block's
original bytes instead, with a small header and checksum. (With `--dedup`
it writes the bytes left after removing repeats, along with the
instructions for putting the repeats back.)

The full specification is the documentation of
[`acz/src/format.rs`](acz/src/format.rs).

The format is version 10 and may change until release 1.0. Each change
increments the version, and streams of older versions are rejected, never
misread.

## License

BSD 3-clause, with one added condition: the dedication to Glen G. Langdon
must be kept in all copies and derivative works. See [LICENSE](LICENSE).
