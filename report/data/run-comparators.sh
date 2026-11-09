#!/bin/bash
# Silesia, every file x codec x repetition, pinned to CPU 7 (Cortex-X925),
# single-threaded. One TSV row per attempt; every exit status recorded.
set -u -o pipefail
cd ~/acz-eval
A=$PWD/acz-tip
D=$PWD/silesia
T=$PWD/sil-scratch
CPU=7
codecs=(acz0 acz1 acz2 acz3 acz3dedup xz9 zstd19 bzip2_9 gzip9)
enc() { case $1 in
  acz0) echo "$A -0 -T1 -c";; acz1) echo "$A -1 -T1 -c";; acz2) echo "$A -2 -T1 -c";;
  acz3) echo "$A -3 -T1 -c";; acz3dedup) echo "$A -3 --dedup -T1 -c";;
  xz9) echo "xz -9 -T1 -c";; zstd19) echo "zstd -19 -T1 -q -c";;
  bzip2_9) echo "bzip2 -9 -c";; gzip9) echo "gzip -9 -c";; esac; }
dec() { case $1 in
  acz*) echo "$A -d -T1 -c";; xz9) echo "xz -d -T1 -c";; zstd19) echo "zstd -d -q -c";;
  bzip2_9) echo "bzip2 -d -c";; gzip9) echo "gzip -d -c";; esac; }
printf "rep\tfile\tbytes\tsha256\tcodec\tpacked\tenc_user\tenc_sys\tenc_wall\tenc_status\tdec_user\tdec_sys\tdec_wall\tdec_status\tcmp\n"
for rep in 1 2 3; do
  for f in $(ls $D); do
    in=$D/$f; bytes=$(stat -c %s $in); h=$(sha256sum < $in | cut -c1-64)
    for c in "${codecs[@]}"; do
      /usr/bin/time -f "%U\t%S\t%e" -o $T/te taskset -c $CPU $(enc $c) < $in > $T/p; es=$?
      packed=$(stat -c %s $T/p)
      /usr/bin/time -f "%U\t%S\t%e" -o $T/td taskset -c $CPU $(dec $c) < $T/p > $T/o; ds=$?
      cmp -s $in $T/o && ok=ok || ok=DIFF
      printf "%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n" $rep $f $bytes $h $c $packed "$(tail -1 $T/te)" $es "$(tail -1 $T/td)" $ds $ok
      rm -f $T/p $T/o
    done
  done
done
