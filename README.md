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
| `--no-dedup` | do not remove long repeats (the default) |
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

| model | predicts each byte from | compress | decompress |
|---|---|---:|---:|
| `-4` (default) | many contexts, mixed (context mixing, the PAQ family) | 1.3 MB/s | 1.2 MB/s |
| `-3` | the longest context that has seen it, up to 6 bytes (PPMd variant H) | 11.9 MB/s | 10.4 MB/s |
| `-2` | the previous two bytes, one bit at a time | 33.6 MB/s | 22.2 MB/s |
| `-1` | the previous byte | 24.4 MB/s | 22.1 MB/s |
| `-0` | byte frequencies alone | 21.4 MB/s | 20.2 MB/s |

The speeds are input bytes per CPU second (decimal MB/s), for one
Cortex-X925 core on the Silesia corpus (below). Level 3 uses the measured
format-v10 PPMd-H implementation at `debc62e`; the other levels retain the
format-v9 measurements of their unchanged model and coder. Blocks can be
coded independently by multiple workers; actual scaling depends on the
input size, memory budget, and machine.

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

## Results

### Silesia benchmark

Level 3 uses the format-v10 PPMd-H run at `debc62e`. Other acz rows retain
the format-v9 measurements; they were not retimed with this build. The
original custom PPM measurements will accompany the technical report.

The Silesia corpus (Sebastian Deorowicz, Silesian University of
Technology, 2003) is a standard benchmark for compressors: 12 files,
211,938,580 bytes in all, of the kinds of data people actually store:
English and Polish text, a dictionary, XML, source code (samba), executable
programs (mozilla, ooffice), a database (osdb), chemical structure data
(nci), medical images (mr, x-ray) and star-catalogue data (sao).

Each file was compressed separately, on one Cortex-X925 core with every
program single-threaded, at the settings below. Memory use was not
equalized. Times are user plus system CPU seconds for the whole corpus,
the median of three runs. Every round trip was checked byte for byte.

| program | how it compresses | bytes | % of input | compress | decompress |
|---|---|---:|---:|---:|---:|
| acz `-4` | context mixing, arithmetic coding | 42,764,432 | 20.18% | 164.07 s | 170.55 s |
| acz `-4 --dedup` | the same, long repeats removed first | 42,796,316 | 20.19% | 156.43 s | 161.85 s |
| xz 5.4.5 `-9` | LZ77 matches (LZMA2) with a binary range coder | 48,795,480 | 23.02% | 63.89 s | 1.60 s |
| acz v10 `-3` | PPMd-H, byte-normalized range coding | 49,224,897 | 23.23% | 17.82 s | 20.33 s |
| zstd 1.5.5 `-19` | LZ77 matches with Huffman and tabled ANS (FSE) codes | 53,024,529 | 25.02% | 60.52 s | 0.18 s |
| bzip2 1.0.8 `-9` | Burrows–Wheeler transform with Huffman codes | 54,506,769 | 25.72% | 9.22 s | 3.96 s |
| gzip 1.12 `-9` | LZ77 matches (Deflate) with Huffman codes | 67,631,918 | 31.91% | 11.37 s | 0.75 s |
| acz `-2` | previous two bytes, arithmetic coding | 69,610,084 | 32.84% | 6.31 s | 9.55 s |
| acz `-1` | previous byte, arithmetic coding | 94,356,023 | 44.52% | 8.69 s | 9.60 s |
| acz `-0` | byte frequencies, arithmetic coding | 127,167,804 | 60.00% | 9.92 s | 10.48 s |

The LZ77 programs decode fast because decoding mostly copies earlier
output; acz's decoders must repeat every prediction the encoder made.

acz `-4` is 12.4% smaller than xz `-9` in total and smaller on every file
(by 3.7% on `sao` to 21% on `reymont`). It is 2.6 times slower to compress
and about 100 times slower to decompress. PPMd-H `-3` produces 15.1% more
bytes than `-4`, with 9.2 times its encoding throughput and 8.4 times its
decoding throughput. It produces 7.2% fewer bytes than zstd `-19`.

On the same core, 7-Zip 23.01 PPMd at order 6 and 256 MiB produces
49,029,343 bytes in 15.77 CPU seconds and decodes in 16.90 seconds.
The acz total is 0.4% larger. Both implement PPMd-H, but acz starts a fresh
model every 16 MiB and sizes its arena from the block length, while 7-Zip
codes each file as a single stream. The formats also have different overheads.

The technical report and reproducibility data will be added after the
remaining measurements and paper reviews are complete. They include input
hashes, build identities, scripts, and measured rows, with earlier runs
retained separately.
A full-corpus size pilot of shorter copy thresholds found a small benefit
for PPMd at 128 bytes but a size penalty for CM; the 256-byte threshold and
disabled-by-default repeat stage remain unchanged.

### Record contexts

Context mixing detects fixed-length records and predicts each byte from
the corresponding positions in preceding records. These contexts are
included in the current `-4` results above and require no additional option.

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
