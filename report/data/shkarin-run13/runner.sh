#!/bin/bash
# Development-corpus size run of the frozen v6 source d1c04ca3 (adds seef: the
# escape classes learn the escape frequency, Shkarin 3.3 and 5.5): every configuration encoded,
# decoded and compared (with the canonical-end check), code strings dumped,
# hashed and kept. Not a timing run: encode_s and verify_s are reported
# separately and only for orientation.   v6-run-d1c04ca3.sh OUTDIR
#
# The snapshot src/claude-v6-frozen-d1c04ca3 is unmodified (MANIFEST.sha256).
# It is built in src/claude-v6-build-d1c04ca3, a copy whose Cargo.toml points
# at hathor's arith (../acz-v9-base/arith; same code, the dedication comment
# differs) and whose Cargo.lock cargo rewrote for that crate's version; both
# differences are recorded below.
set -u -o pipefail
O=$1; case $O in /*) ;; *) echo "OUTDIR must be absolute" >&2; exit 2;; esac; S=~/acz-eval/devcorpus; T=~/acz-eval/tree-20261108/src
F=$T/claude-v6-frozen-d1c04ca3; R=$T/claude-v6-build-d1c04ca3; B=$R/target/release/devbench
A=$T/acz-v9-base
mkdir -- "$O" || exit 2
CONFIGS="base see2 seef bin6+par+inh+see2 bin6+par+inh+seef"
{ date -u; uname -a; rustc --version
  sha256sum $B $F/MANIFEST.sha256 $F/src/ppm6.rs $F/src/bin/devbench.rs $R/Cargo.toml $R/Cargo.lock "$0"
  (cd $F && sha256sum -c MANIFEST.sha256 | grep -vc ': OK$' | sed 's/^/snapshot manifest mismatches: /')
  diff -r $F/src $R/src > /dev/null && echo "build src identical to snapshot src"
  echo "build differs from snapshot:"; diff $F/Cargo.toml $R/Cargo.toml; diff $F/Cargo.lock $R/Cargo.lock
  echo "arith dependency:"; sha256sum $A/Cargo.toml $A/arith/Cargo.toml $A/arith/src/*.rs
  echo "configs: $CONFIGS"; echo "inputs: $S (files-sha256.txt there)"
  echo "other processes on core 7:"; ps -eo psr,pcpu,comm --no-headers | awk '$1==7 && $2>1.0'; } > "$O/env.txt"
cd $S && find cantrbry large calgary enwik8 -type f | sort | xargs taskset -c 7 $B --verify --dump "$O/code" $CONFIGS -- > "$O/sizes.tsv"; echo "status $?" >> "$O/env.txt"
(cd "$O/code" && find . -type f | sort | xargs sha256sum) > "$O/code-sha256.txt"
chmod -R a-w "$O/code"
echo done > "$O/done"
