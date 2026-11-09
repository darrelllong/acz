#!/bin/bash
# acz -3 on Silesia: v9 (2a73c2b, linear hashing) against trie v4 (the v9
# model on the context trie, ppm4.rs ca81ea6c). X925 core 7, four repetitions
# per file, v9 first on repetitions 1 and 4, v4 first on 2 and 3. Each row:
# UTC start and end, other processes on core 7 at the start, exit statuses,
# GNU time user, system and elapsed seconds and max RSS (KiB) for encoding
# and decoding, output sha256 and cmp.
#   tree-run3.sh OUTDIR   (OUTDIR must be new)
set -u -o pipefail
[ "$#" -eq 1 ] || { echo "usage: $0 OUTDIR" >&2; exit 2; }
O=$1; S=~/acz-eval/silesia; R=~/acz-eval/tree-20261108/src
declare -A BIN=([v9]=$R/acz-v9-base/target/release/acz [tree4]=$R/acz-v9-tree4/target/release/acz)
mkdir -- "$O" || exit 2
T=$(mktemp -d); trap 'rm -rf "$T"' EXIT
{ date -u; uname -a; sha256sum "${BIN[v9]}" "${BIN[tree4]}" "$0"; ps -eo psr,pcpu,args --sort=-pcpu | head -15; } > "$O/env.txt"
echo -e "start_utc\tend_utc\trep\tfile\tbuild\tbytes\tout_sha256\tenc_user\tenc_sys\tenc_elapsed\tenc_rss_kib\tenc_status\tdec_user\tdec_sys\tdec_elapsed\tdec_rss_kib\tdec_status\tcmp\tcore7_others" > "$O/rows.tsv"
orders=("v9 tree4" "tree4 v9" "tree4 v9" "v9 tree4")
for f in "$S"/*; do for r in 1 2 3 4; do
  for b in ${orders[$((r - 1))]}; do
    B=${BIN[$b]}
    others=$(ps -eo psr,pcpu,comm --no-headers | awk '$1==7 && $2>1.0 {printf "%s:%s,", $3, $2}')
    st=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    /usr/bin/time -f "%U %S %e %M" -o "$T/t" taskset -c 7 "$B" -3 -T1 -c "$f" > "$T/c"; es=$?; read eu esy ee erss < "$T/t"
    /usr/bin/time -f "%U %S %e %M" -o "$T/t" taskset -c 7 "$B" -d -T1 -c "$T/c" > "$T/o"; ds=$?; read du dsy de drss < "$T/t"
    cmp -s "$f" "$T/o" && ok=ok || ok=DIFF
    en=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    echo -e "$st\t$en\t$r\t$(basename "$f")\t$b\t$(stat -c %s "$T/c")\t$(sha256sum < "$T/c" | cut -c1-64)\t$eu\t$esy\t$ee\t$erss\t$es\t$du\t$dsy\t$de\t$drss\t$ds\t$ok\t${others:-none}" >> "$O/rows.tsv"
  done
done; done
echo done > "$O/done"
