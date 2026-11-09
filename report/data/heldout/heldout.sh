#!/bin/bash
# Held-out evaluation, following Darrell's protocol: for each selected file,
# copy it to the measuring host's home directory, uncompress it there,
# measure each codec, and remove it before the next file.
#
#   heldout.sh LIST OUTDIR [HOST...]
#
# LIST holds one source path per line (on /gilgamesh or /enkidu, which are
# only read). The files are dealt to the hosts in turn (default knuth paris
# baase vinge), and each host works through its share one file at a time,
# so the four hosts run in parallel. measure.sh does the per-file work
# (copy, uncompress, measure, verify, remove) and prints one TSV row per
# codec; each host's rows go to OUTDIR/HOST.tsv.
#
# measure.sh and the acz binary must already be in ~/acz-bench on each
# host (ACZ_BIN, an absolute path there, names another acz). Set
# CODECS to choose the codecs (default: acz4 acz3 xz9 zstd19)
# and ACZ_REPS the timed runs per codec (default 3). ACZ_CPUS lists the
# cores to use on every host (default 7): each core runs its own worker,
# pinned to it, so a host measures that many files at once. The files are
# dealt to the workers in turn, so a LIST sorted by size, largest first,
# balances the load. Each worker's rows go to OUTDIR/HOST-cpuN.tsv. An
# entry may be a range such as 5-8, for a worker given several cores; set
# ACZ_THREADS to match, so acz uses them.
# OUTDIR must be new, so an earlier measurement cannot be overwritten.
# The current worker contract is one complete 20-column row per file and
# codec. Transport errors, incomplete inputs and missing rows fail the run.
set -euo pipefail
[ "$#" -ge 2 ] || { echo "usage: $0 LIST OUTDIR [HOST...]" >&2; exit 2; }
list="$1"; out="$2"; shift 2
[ -f "$list" ] && [ -r "$list" ] || { echo "cannot read file list: $list" >&2; exit 2; }
hosts=("$@"); [ ${#hosts[@]} -gt 0 ] || hosts=(knuth paris baase vinge)
seen=" "
for h in "${hosts[@]}"; do
  [[ "$h" =~ ^[A-Za-z0-9_][A-Za-z0-9_.@-]*$ ]] || { echo "invalid host: $h" >&2; exit 2; }
  case "$seen" in *" $h "*) echo "duplicate host: $h" >&2; exit 2;; esac
  seen="$seen$h "
done
read -r -a codecs <<< "${CODECS:-acz4 acz3 xz9 zstd19}"
seen=" "
for c in "${codecs[@]}"; do
  case "$c" in acz4|acz3|xz9|zstd19) ;; *) echo "unknown codec: $c" >&2; exit 2;; esac
  case "$seen" in *" $c "*) echo "duplicate codec: $c" >&2; exit 2;; esac
  seen="$seen$c "
done
[ ${#codecs[@]} -gt 0 ] || { echo "no codecs selected" >&2; exit 2; }
mkdir -p -- "$(dirname -- "$out")"
mkdir -- "$out" || { echo "output directory must be new: $out" >&2; exit 2; }
cp -- "$list" "$out/source-list.txt"
awk 'NF { print }' "$out/source-list.txt" > "$out/list.txt"
[ -s "$out/list.txt" ] || { echo "file list is empty" >&2; exit 2; }

read -r -a cpus <<< "${ACZ_CPUS:-7}"
reps="${ACZ_REPS:-3}"
[[ "$reps" =~ ^[1-9][0-9]*$ ]] || { echo "invalid ACZ_REPS: $reps" >&2; exit 2; }
[ ${#cpus[@]} -gt 0 ] || { echo "no CPUs selected" >&2; exit 2; }
seen=" "
for c in "${cpus[@]}"; do
  [[ "$c" =~ ^[0-9]+(-[0-9]+)?$ ]] || { echo "invalid cpu or range: $c" >&2; exit 2; }
  case "$seen" in *" $c "*) echo "duplicate cpu or range: $c" >&2; exit 2;; esac
  seen="$seen$c "
done
workers=()
for h in "${hosts[@]}"; do
  for c in "${cpus[@]}"; do workers+=("$h-cpu$c"); done
done
n=${#workers[@]}
pids=()
for i in "${!workers[@]}"; do
  w=${workers[$i]}; h=${w%-cpu*}; c=${w##*-cpu}
  awk -v n="$n" -v i="$i" '(NR - 1) % n == i' "$out/list.txt" > "$out/$w.list"
  {
    host_status=0
    while IFS= read -r f; do
      printf -v command 'CODECS=%q ACZ_CPU=%q ACZ_REPS=%q ACZ_THREADS=%q ACZ_CODEC_CPUS=%q ~/acz-bench/measure.sh %q unpack' \
        "${codecs[*]}" "$c" "$reps" "${ACZ_THREADS:-1}" "${ACZ_CODEC_CPUS:-}" "$f"
      # ACZ_BIN is an absolute path on the measuring host.
      [ -n "${ACZ_BIN:-}" ] && printf -v command 'ACZ=%q %s' "$ACZ_BIN" "$command"
      if ssh -n "$h" "$command"; then
        :
      else
        rc=$?
        printf '%s: remote measurement exited %s for %s\n' "$w" "$rc" "$f" >&2
        host_status=1
      fi
    done < "$out/$w.list"
    exit "$host_status"
  } > "$out/$w.tsv" 2> "$out/$w.err" &
  pids+=("$!")
done

status=0
for i in "${!pids[@]}"; do
  if ! wait "${pids[$i]}"; then
    echo "${workers[$i]}: remote measurement failed; see ${workers[$i]}.err" >&2
    status=1
  fi
done
for h in "${workers[@]}"; do
  if ! awk -F'\t' -v host="$h" -v files="$(wc -l < "$out/$h.list")" -v codecs="${codecs[*]}" -v reps="$reps" '
    function timing_ok(value, parts, count, j) {
      count = split(value, parts, ",")
      if (count != reps) return 0
      for (j = 1; j <= count; j++)
        if (parts[j] !~ /^[0-9]+([.][0-9]+)?$/) return 0
      return 1
    }
    BEGIN { n = split(codecs, names, " "); for (i = 1; i <= n; i++) expected[names[i]] = files }
    {
      rows++
      if (NF != 20 || $6 != "0" || $7 != "yes" || $15 != "0" || $19 != "0" || $20 != "ok" || !($10 in expected)) bad++
      if (!timing_ok($12) || !timing_ok($13) || !timing_ok($14) ||
          !timing_ok($16) || !timing_ok($17) || !timing_ok($18)) bad_times++
      counts[$10]++
      if (seen[$2 SUBSEP $10]++) duplicates++
      if (!paths[$2]++) path_count++
    }
    END {
      for (codec in expected) if (counts[codec] != expected[codec]) mismatches++
      if (path_count != files) mismatches++
      printf "%s: rows=%d expected=%d invalid=%d bad_timings=%d duplicates=%d count_mismatches=%d\n", host, rows, files*n, bad, bad_times, duplicates, mismatches
      exit (bad || bad_times || duplicates || mismatches)
    }' "$out/$h.tsv"; then
    status=1
  fi
done
exit "$status"
