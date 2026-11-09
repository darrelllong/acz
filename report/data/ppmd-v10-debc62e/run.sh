#!/bin/bash
# Silesia: acz -3 (PPMd-H port) against 7-Zip 23.01 PPMd (order 6, 256 MB,
# one thread), core 7, three alternating repetitions per file. Every row
# records its repetition, input and output hashes, and both exit statuses.
set -u -o pipefail
S=~/acz-eval/silesia; B=~/acz-eval/src-final/acz/target/release; O=~/acz-eval/diag-final; T=$(mktemp -d)
trap "rm -rf $T" EXIT
cd "$S"
{ cat ~/acz-eval/src-final/acz/REV; sha256sum $B/acz $B/examples/ppmd_restarts "$0"; 7z i | sed -n 2p; uname -a; lscpu | grep -m1 "Model name"; } > $O/env.txt
taskset -c 7 $B/examples/ppmd_restarts * > $O/restarts.tsv
echo -e "rep\tfile\tinput_sha256\tcodec\tbytes\toutput_sha256\tenc_user\tenc_sys\tenc_status\tdec_user\tdec_sys\tdec_status\tcmp" > $O/rows.tsv
t() { /usr/bin/time -f "%U %S" -o $T/t taskset -c 7 "$@"; }
for f in *; do
  ih=$(sha256sum < $f | cut -c1-64)
  for r in 1 2 3; do
    t $B/acz -3 -T1 -c $f > $T/a; es=$?; read eu esy < $T/t
    t $B/acz -d -T1 -c $T/a > $T/o; ds=$?; read du dsy < $T/t
    cmp -s $f $T/o && ok=ok || ok=DIFF
    echo -e "$r\t$f\t$ih\tacz3\t$(stat -c %s $T/a)\t$(sha256sum < $T/a | cut -c1-64)\t$eu\t$esy\t$es\t$du\t$dsy\t$ds\t$ok" >> $O/rows.tsv
    rm -f $T/p.7z; t 7z a -bd -bso0 -bsp0 -t7z -m0=PPMd:o=6:mem=256m -mmt=1 $T/p.7z $f; es=$?; read eu esy < $T/t
    t 7z e -bd -bsp0 -so $T/p.7z > $T/o; ds=$?; read du dsy < $T/t
    cmp -s $f $T/o && ok=ok || ok=DIFF
    echo -e "$r\t$f\t$ih\tppmd-o6-256m\t$(stat -c %s $T/p.7z)\t$(sha256sum < $T/p.7z | cut -c1-64)\t$eu\t$esy\t$es\t$du\t$dsy\t$ds\t$ok" >> $O/rows.tsv
  done
done
echo done > $O/done
