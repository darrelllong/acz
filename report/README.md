# Technical report

Edit `report.tex`. Build the paper from the repository root:

```sh
python3 report/build_report.py
```

The build requires Python 3 and `pdflatex` with the standard TeX Live/MacTeX
packages used by the document (including newtx, microtype, titlesec, placeins,
TikZ and PGFPlots). It uses
only the Python standard library. It writes the PDF to
`output/pdf/arith-acz-technical-report-draft.pdf` and temporary products to
`build/`. It also refreshes the ignored `report.pdf` beside the LaTeX
source, so a direct-open PDF cannot remain on an older report. The established
output filename is retained so existing links work.
The build never rewrites the paper's prose or runs benchmarks.

Headings follow the [IEEE Editorial Style Manual for Authors](https://journals.ieeeauthorcenter.ieee.org/wp-content/uploads/sites/7/IEEE-Editorial-Style-Manual-for-Authors.pdf),
page 11: centered small capitals with Roman numerals for sections, lettered
italic subsections, and numbered italic run-in tertiary headings. The
single-column page layout is retained for reading the draft.

Tables and comparison figures are generated from `data/`. The Kiviat
diagram uses compressed size on a common 0–100% of input scale across all twelve files; the
time plots use measured CPU seconds on logarithmic horizontal axes. Before typesetting, the build checks that
all 396 selected attempts exist exactly once, all processes succeeded, every
round trip matched, input sizes and SHA-256 identities agree, and compressed
sizes are stable across repetitions. The selection is the original
experiment's 324 attempts for every configuration but level 3, the selected
model and coder's 36 attempts from run 5, and 36 attempts
of 7-Zip's PPMd. It sums bytes once per configuration; for each timing it
sums user and system CPU seconds over the corpus per repetition and takes
the median of those three totals. The historical trie/hash table retains its
separate four-repetition calculation. Decimal arithmetic
preserves the recorded timing precision. A missing attempt or typesetting
error fails the build rather than silently producing a partial report.

## Retained evidence

### Current level 3: model and coder, run 5

`data/shkarin-range-run5/` retains the exact 108-row Silesia comparison of
the selected model/coder, preceding trie, and historical `debc62e` format-10
port. `build_current_results.py` validates its manifest, input and output
identities, complete 12 × 3 × 3 coverage, statuses, round trips, timing values,
and Latin-square order. Each build occupies each position twelve times; this
is not all six permutations. The main report selects the new build's 36 rows;
a separate table gives all three builds, including elapsed times and maximum
observed process peak RSS. The port's level-3 model and coder are those of
release 0.5.0: `acz/src/ppmd.rs` and `acz/src/models.rs` differ between
`debc62e` and release root `fab6466` only in the dedication comment. Thus the
size tradeoff also matters to 0.5.0 users at matching settings with repeat
removal off. The retained version-10/version-11 checks below preserve the
tested payloads and sizes in that configuration; the complete public 0.5.0
executable was not timed in run 5.

The timed source is snapshot `c7aef7e0`, private format 12. `measured-source/`
contains its runtime source and Cargo files; the other implementations are
identified by hashes and retained privately. No port source is included here.
`runner.sh` records the original paths and exact commands. Reproduction needs
the matching inputs and baseline binaries/sources at adapted paths; building
the current checkout alone does not reproduce all historical configurations.
Source, input and binary identities were independently captured after the
run. The per-case filtered process snapshots do not prove continuous core
occupancy. The runner deletes its temporary outputs after hashing and checking
them. Independent local encodings subsequently reproduced all twelve recorded
new-container hashes and decoded to the original input hashes.

`format13-cases.json` records a separate identity check: matching version-13
containers decode correctly with unchanged sizes. Independently rehashed
retained version-12/version-13 pairs differed only in the version byte and
eight-byte header CRC, which was checked by a separate bit-at-a-time CRC64/XZ
implementation. Both incompatible private version-12 variants are explicitly
rejected, including at concatenated-member boundaries. The runtime integration
`0d8dace` matches reviewed snapshot `068f1d24`; this check does not retime it.
The manifest gives full source identities. The two incompatible private
version-12 decoders and all original measurements remain preserved privately.

### Model-selection sizes, run 21

The component table also uses `data/shkarin-run11/`, `shkarin-run13/`, and
`shkarin-run18/`, from frozen sources `a8a56841`, `d1c04ca3`, and `c9b15f27`.
The build validates all configurations in each run, not just displayed rows:
759 outputs across these three runs and run 21. Shared configurations must
match in actual independently captured output hashes as well as sizes.
The records preserve the individual and combined features, immediate versus
delayed new-context inheritance, and per-file regressions. The original
class probability learner and the selected class frequency learner are
distinct alternatives. These size effects are not additive or causal
decompositions of the historical port gap.

`data/shkarin-run21/` retains all four configurations and 132 raw-code sizes
from frozen experimental source `c60f81f4`, with the older general interval
coder. Its 33 input entries total 117,221,759 bytes but have 32 unique contents:
`calgary/pic` equals `cantrbry/ptt5`. All original rows and the duplicate weight
are retained. Input and output contents were independently rehashed after the
run. The original harness verified every decoded byte and canonical termination;
the independent audit did not decode those outputs again. Fixed-order
`encode_s` and `verify_s` columns are retained as records, not used for
comparative timing or an isolated model-overhead claim.

The selected five changes produce 27,522,780 raw bytes; `geo` is the one
regression against the baseline. Adding EE1, our first-symbol-rank-zero
approximation to the paper's Section 3.2 initializer, saves 13,787 aggregate
bytes but worsens 31/33 entries. This is not a test of the full numerical
initializer. EE1 remains off. Historical experimental sources and all preceding
ablations, rejected variants and correction records are preserved privately;
reproduction of this size run requires the source identified by its manifest.

### Earlier level 3: the context trie

`data/trie-v4-run3/` is the balanced Silesia run of one PPM model with its
contexts in a hash table (`v9`, 2a73c2b) and in the trie (`tree4`, the
frozen `ppm4.rs`, sha256 ca81ea6c…), four repetitions, each build first in
two, on hathor's core 7, with CPU, elapsed time and peak RSS per process.
`build_tree_results.py` validates it (evidence hashes, order, statuses,
round trips, output identities); `build_report.py` retains it as the earlier
trie-versus-hash comparison. It is not selected as the current level 3.
Its code strings equal the first private format-12 trie integration, with
only header differences. They differ from the current model/coder payloads.

`data/trie-v4-blocks/` records, for each 16 MiB block, both models' code
sizes and restart counts, whether the code strings are identical, and the
trie's round trip (`bench compare` from the experiment crate, copied from
`hathor:~/acz-eval/tree-20261108/bench-run1/` with its script and the bench
source that was built). The build checks every block and requires identical
code strings wherever neither model restarts.

### Retained evidence of earlier level-3 models

The hashed-context PPM's original row (`data/ppm.tsv`), its diagnostic, and
the PPMd-H port's format-10 experiment remain in `data/` and are still
validated, but none is selected as level 3. The format-10 experiment supplies
only its 7-Zip PPMd rows, as a reference. The dedup experiments below
measured the port as model 3; the report tabulates their CM results only.

The separate `data/dedup-v11-69315de/` experiment records 24 complete
Silesia containers, one per file and model (PPMd-H or CM), with repeat
removal enabled. Its measured sizes are not substituted into the earlier
timing table. The build validates the complete matrix, successful round
trips, corpus identities, and evidence hashes before generating its table.
`manifest.json` identifies the compiler, binary, settings, and source;
`source.tar.gz` preserves the exact runtime sources and Cargo files from
69315de.

Three source archives of this evidence contain the PPMd-H port that 0.6.0
removed, so the public repository omits them; they are retained unchanged
in the private history, and their SHA-256 hashes identify them:

| Archive | SHA-256 |
| --- | --- |
| `data/dedup-v11-69315de/source.tar.gz` | `4b41a1ca7f71ec7e7da618b00ac684a3f52f0bdb834b4b227ac5fd19225c29b7` |
| `data/dedup-v11-69315de/v10-source.tar.gz` | `2672c77ca4bfe29e362570e62b70f450f97fc9d2d28dbfdc6cc72f1f5e226472` |
| `data/dedup-size-pilot-source.tar.gz` | `603f0720850a794c05d52f2b9caf5a5ab251d4c4332515373130d4f9d885f8e1` |

The first is recorded in `manifest.json`, the second in its `identity`
section. The build checks the first archive's hash when the file is present
and reports its absence otherwise. Every other evidence file consumed by
the build remains required. `original-run.py` preserves the actual local
invocation paths. Repeating these historical measurements requires access
to the privately retained source snapshot. With that snapshot, extract it,
build with `cargo build --release`, and run its `target/release/acz` with `-3` or `-4`,
`--dedup=256M --block=16M -T1 --memory=4G -c` on each Silesia input. Decode
each result with `-d -T1 --memory=4G -c` and compare it with its input.

The same directory retains the 70-case dedup-off identity results and
original script. `v10-source.tar.gz` preserves the debc62e baseline for
that check; the code compares bytes after the 16-byte header, verifies
the unchanged header parameters, and round-trips the v11 result. These
checks establish payload identity for the recorded cases, not equal
runtime. The source archives intentionally contain no Git history or
working reviews. All v9/v10 timing and threshold-pilot evidence below is
retained unchanged.

The following names identify the original measurement artifacts. Host paths
in the retained evidence record provenance; they are not portable commands:

| Local file | Original artifact | Selected data |
| --- | --- | --- |
| `data/acz.tsv` | `silesia-v9/silesia-v9.tsv` | All configurations except `acz3`: 180 rows |
| `data/ppm.tsv` | `silesia-v9/silesia-final-ppm.tsv` | Historical custom PPM only: 36 rows |
| `data/ppmd-v10-debc62e/rows.tsv` | `ppmd-reference/silesia-final-debc62e/rows.tsv` | Historical PPMd-H port: 36 acz rows; 36 reference rows used separately |
| `data/comparison-source.tsv` | `silesia-v8/silesia-v8.tsv` | xz, zstd, bzip2 and gzip only: 144 rows |

The comparator artifact also contains older acz measurements; none is used
in the paper. The historical custom PPM run retains its timings after the symbol-array
copy improvement; it no longer supplies the selected level-3 row. The retained environment files record executable hashes,
source revisions and tool versions; the three run scripts record the exact
commands and ordering. Their original executable names and host paths are
preserved as evidence, not configured as portable benchmark commands.

`data/host-hardware.txt` identifies the benchmark host as an NVIDIA DGX
Spark with a GB10 processor and maps logical CPU 7 to a Cortex-X925 core.
It was collected after the timing runs. It records hardware identity and
frequency capabilities, not measured clock rates during those runs.

### Historical custom PPM diagnostic

The original custom-PPM/PPMd comparison and restart counts are a later pass on the same host.
They do not replace any original timing rows. The build reads
`data/ppm-diagnostic.tsv` and `data/ppm-restarts.tsv`, checks all 72 exact
round trips, stable sizes, and all 19 blocks, and verifies that acz sizes
and framed diagnostic payloads reproduce the original PPM sizes. It writes
`build/ppm-diagnostic.tex` and `build/ppm-diagnostic.json` separately.

The files were copied unchanged from `hathor:~/acz-eval/diag/`: `ppmd.tsv`,
`env.txt`, and `run.sh` have the local prefix `ppm-diagnostic`; the corrected
`restarts-13cf1ad.tsv` and `.env` have the local prefix `ppm-restarts`.
The comparison encoder is revision `b42d490`; the corrected diagnostic is
`13cf1ad`, which changes the example but not the encoder. The retained
command script initially ran the older restart example; its restart output
is not used. The later corrected output is the retained restart evidence.

The raw timing file omits repetition IDs and process exit-status columns.
The retained script establishes the order: three acz/PPMd pairs per file.
The build requires that ordering before assigning repetition numbers and
checks finite nonnegative timings and exact round-trip results. Corpus
times use the median of the three corpus sums, not a sum of per-file medians.
`data/ppm-diagnostic-input-check.txt` records a subsequent SHA-256 check
against the original corpus; it was not captured within each timed attempt.
PPMd's settings are `-t7z -m0=PPMd:o=6:mem=256m -mmt=1`, one archive per
file; acz uses `-3 -T1` and its default 16 MiB blocks. Archive formats,
memory allocations, and block/reset policies differ.

Run the diagnostic evidence checks with:

```sh
python3 -m unittest discover -s report -p 'test_*.py'
```

### Format-v10 PPMd-H experiment (retained; supplies the 7-Zip reference)

`data/ppmd-v10-debc62e/` is copied from `hathor:~/acz-eval/diag-final/`.
The selected build is `debc62eb5f8469e712dca15d7413f22460c24942`.
`env.txt` records its executable and script hashes; `rows.tsv` records explicit
repetition IDs, both input and output SHA-256 hashes, both exit statuses,
and round-trip results for 72 attempts. `restarts.tsv` records all 19 blocks.
`compiler-postrun.txt` is a subsequent compiler-version check, including
the compiler cache from the actual build directory, not a timestamped record
from inside the run. All commands and original host paths remain unchanged.

The build validates every attempt, input identity, output-hash and size
stability, block coverage, and payload-plus-framing totals. Its acz rows,
the PPMd-H port, are no longer selected; its 36 7-Zip rows supply the
reference row of the totals table. CPU times are medians of corpus
sums, not sums of file medians. The 7-Zip settings remain order six and
256 MiB, one archive per file, while acz resets every 16 MiB and varies
its arena with the block length. Estimator/coder compatibility does not
make these container and memory configurations identical.

`data/ppmd-v10-9268e3f/` preserves the preceding complete run, including
its slower decode result; none of its timings is silently overwritten.
Each retained experiment has its own build identity. `PPMD_RUN` in `build_report.py` names the run that
supplies the 7-Zip reference; the generated `build/metrics.tex` supplies every
level-3 and reference number in the paper's prose.

### Minimum-copy size pilot

`data/dedup-size-pilot.tsv` retains all 190 block/configuration rows for
19 Silesia blocks, two models, and five settings (off, 32, 64, 128, 256).
The input identities, frozen source manifest, and completed validation
summary are saved alongside it. The privately retained
`dedup-size-pilot-source.tar.gz`, identified by its SHA-256 above, contains
the exact source and runner. Repeating this historical experiment requires
that snapshot. With it, extract into an empty scratch directory and set
`CORPUS` in `run.py` to the same Silesia inputs; the retained input
hashes establish their identity. The experiment uses a fixed 32-byte
fingerprint, unchanged sparse anchors, 16 MiB blocks, and a 256 MiB window.

The harness measures projected container length from actual residual
encoding, commands, length fields, and fixed framing. It verifies every
residual decode and full reconstruction, but does not serialize complete
checksummed files for each experimental threshold. Full-program controls
check the disabled-stage totals, the CM 256-byte total, and PPMd framing
on dickens. The source snapshot predates the final timing optimization;
its PPMd bytes agree with the reference and the historical port's full-corpus output.
Recorded local wall times are not used as publication CPU measurements.
The report's table shows the CM column; the pilot's level-3 column measured
the PPMd-H port.

The build validates complete coverage, raw lengths, round trips, stored
versus compressed choices, copy thresholds, framing, input hashes, and
full-program control totals. Its table is a size ablation only. A shorter
threshold would require revisiting product command-buffer admission as
well as demonstrating a benefit on held-out data.

`build/results.json` contains computed totals, and
`build/evidence-sha256.json` hashes every retained evidence file recursively.
The original experiment, selected replacement, and intermediate results
remain separate. Held-out measurements are not pooled with Silesia.

### Held-out collections

`data/heldout/` retains the Census and Backblaze size measurements, one row
per sampled input: its path identifier (the first 16 hex digits of the
SHA-256 of its source path), file type, measured bytes and SHA-256, and the
compressed bytes of `acz -4`, `xz -9`, `zstd -19`, release 0.5.0's level 3
(build `debc62e`) and the selected level 3 (build `a3c9440`, format 13).
`reduce_heldout.py` produced these rows from the raw 20-column rows written
by `measure.sh` under `heldout.sh`; it requires exactly one row per input,
codec and pass, and identical content in both passes. `knuth-sources.sha256`
identifies the raw row files, sample lists and run records it read, which
are retained privately; the `.env` files are the run records.
`build_heldout_results.py` checks the manifest, rejects a duplicate, failed
or missing input, requires 11,058 measured Census inputs plus the one empty
input and all 688 Backblaze inputs, and writes the table.

## Reviewing the PDF

After changing the prose or layout, render and inspect every page:

```sh
mkdir -p report/build/render
pdftoppm -r 120 -png report/output/pdf/arith-acz-technical-report-draft.pdf report/build/render/page
```

Both plots are vector graphics typeset by PGFPlots from generated coordinates.

The build rejects undefined references and overflowing boxes. Visual review
also checks equations, tables, page breaks and reference URLs.
