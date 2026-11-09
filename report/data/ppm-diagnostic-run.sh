#!/bin/bash
# Silesia diagnostic: PPM restarts per block, and PPMd (7-Zip, order 6,
# 256 MB, one thread) against acz -3, on core 7, three runs each.
set -u -o pipefail
S=~/acz-eval/silesia; B=~/acz-eval/src/acz/target/release; O=~/acz-eval/diag; T=$(mktemp -d)
trap "rm -rf $T" EXIT
cd "$S"
{ cat ~/acz-eval/src/acz/REV; sha256sum $B/acz $B/examples/ppm_restarts; 7z i | sed -n 2p; uname -a; } > $O/env.txt
taskset -c 7 $B/examples/ppm_restarts * > $O/restarts.tsv
echo -e "file\tcodec\tbytes\tenc_user\tenc_sys\tdec_user\tdec_sys\tcmp" > $O/ppmd.tsv
t() { /usr/bin/time -f "%U %S" -o $T/t taskset -c 7 "$@"; }
for f in *; do
  for r in 1 2 3; do
    t $B/acz -3 -T1 -c $f > $T/a; read eu es < $T/t
    t $B/acz -d -T1 -c $T/a > $T/o; read du ds < $T/t
    cmp -s $f $T/o && ok=ok || ok=DIFF
    echo -e "$f\tacz3\t$(stat -c %s $T/a)\t$eu\t$es\t$du\t$ds\t$ok" >> $O/ppmd.tsv
    rm -f $T/p.7z; t 7z a -bd -bso0 -bsp0 -t7z -m0=PPMd:o=6:mem=256m -mmt=1 $T/p.7z $f; read eu es < $T/t
    t 7z e -bd -bsp0 -so $T/p.7z > $T/o; read du ds < $T/t
    cmp -s $f $T/o && ok=ok || ok=DIFF
    echo -e "$f\tppmd-o6-256m\t$(stat -c %s $T/p.7z)\t$eu\t$es\t$du\t$ds\t$ok" >> $O/ppmd.tsv
  done
done
echo done > $O/done
