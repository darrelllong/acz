#!/bin/bash
# Silesia, X925 core 7, with the claude-tree bench program (16 MiB blocks,
# a fresh model per block, as acz does).
# 1. compare.tsv: v9 and trie v4 per block: code sizes, model restarts,
#    whether the code strings are identical, and v4's round trip.
# 2. rows.tsv: v4 (bit-wise coder) and v5 (v4's model on the range coder),
#    three repetitions, order alternating; the bench's own encode and
#    decode times, GNU time's peak RSS, other processes on core 7.
#   bench-run.sh OUTDIR   (OUTDIR must be new)
set -u -o pipefail
[ "$#" -eq 1 ] || { echo "usage: $0 OUTDIR" >&2; exit 2; }
O=$1; S=~/acz-eval/silesia; C=~/acz-eval/tree-20261108/src/claude-tree; B=$C/target/release/bench
mkdir -- "$O" || exit 2
T=$(mktemp -d); trap 'rm -rf "$T"' EXIT
{ date -u; uname -a; rustc --version; sha256sum "$B" "$0" "$C"/src/*.rs "$C"/src/bin/*.rs; ps -eo psr,pcpu,args --sort=-pcpu | head -15; } > "$O/env.txt"
taskset -c 7 "$B" compare "$S"/* > "$O/compare.tsv"; echo "compare_status $?" >> "$O/env.txt"
echo -e "start_utc\trep\tfile\tmodel\tbytes\tcode_bytes\trestarts\tencode_s\tdecode_s\tmax_rss_kib\tstatus\tcore7_others" > "$O/rows.tsv"
for r in 1 2 3; do
  order="tree4 tree5"; [ $((r % 2)) -eq 0 ] && order="tree5 tree4"
  for f in "$S"/*; do for m in $order; do
    others=$(ps -eo psr,pcpu,comm --no-headers | awk '$1==7 && $2>1.0 {printf "%s:%s,", $3, $2}')
    st=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    /usr/bin/time -f "%M" -o "$T/t" taskset -c 7 "$B" "$m" "$f" > "$T/o"; s=$?
    row=$(tail -1 "$T/o" | cut -f3-7)
    echo -e "$st\t$r\t$(basename "$f")\t$m\t$row\t$(cat "$T/t")\t$s\t${others:-none}" >> "$O/rows.tsv"
  done; done
done
echo done > "$O/done"
