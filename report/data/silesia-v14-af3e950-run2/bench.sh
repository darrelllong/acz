#!/bin/bash
# bench.sh LABEL BIN LEVEL REPS FILE... : encode and decode each file REPS times on core 7;
# print per-file median user+sys seconds and the size; verify every round trip.
# Extra environment for the measured process comes from BENCH_ENV (e.g. "GLIBC_TUNABLES=...").
set -u
label=$1; bin=$2; level=$3; reps=$4; shift 4
S=~/acz-eval/silesia; T=$(mktemp -d /home/darrell/acz-eval/silesia-v14-af3e950-run2/tmp.XXXXXX); trap 'rm -rf "$T"' EXIT
med() { sort -n | awk '{a[NR]=$1} END {print a[int((NR+1)/2)]}'; }
te=0; td=0; ts=0
for f in "$@"; do
  e=(); d=()
  for ((r=1; r<=reps; r++)); do
    /usr/bin/time -f "%U %S" -o $T/t env ${BENCH_ENV:-} taskset -c 7 $bin $level -T1 -c < $S/$f > $T/q || { echo "FAIL enc $f"; exit 1; }
    e+=($(awk '{printf "%.2f", $1+$2}' $T/t))
    /usr/bin/time -f "%U %S" -o $T/t env ${BENCH_ENV:-} taskset -c 7 $bin -d -T1 -c < $T/q | cmp -s - $S/$f || { echo "FAIL dec $f"; exit 1; }
    d+=($(awk '{printf "%.2f", $1+$2}' $T/t))
  done
  me=$(printf '%s\n' "${e[@]}" | med); md=$(printf '%s\n' "${d[@]}" | med); sz=$(stat -c %s $T/q)
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$label" "$f" "$sz" "$me" "$md" "$(IFS=,; echo "${e[*]}")" "$(IFS=,; echo "${d[*]}")"
  te=$(echo "$te + $me" | bc); td=$(echo "$td + $md" | bc); ts=$((ts + sz))
done
printf '%s\tTOTAL\t%s\t%s\t%s\n' "$label" "$ts" "$te" "$td"
