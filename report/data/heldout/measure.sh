#!/bin/bash
# acz evaluation worker: measure one input and print one TSV row per codec.
#
#   measure.sh <path> [unpack]
#
# The source is only read. With "unpack", the source is copied to scratch;
# a .bz2/.zst/.gz/.xz source is decompressed there, completely, and a .zip
# has all its members extracted, concatenated in archive order; the whole
# result is what is measured. Any other source is measured as copied (the
# unpack column says "copy"). If decompression fails, nothing is
# measured. Setting ACZ_PREFIX=N measures only the first N decompressed
# bytes instead; such rows say "prefix:N" in the unpack column and "no" in
# the complete column, so they cannot be mistaken for whole-file rows.
# Scratch is under $ACZ_SCRATCH (default ~/acz-bench/tmp), removed on exit.
#
# Every outcome, including failure, is one 20-column row per codec. Each
# codec is run ACZ_REPS times (default 3) each way; the six time columns
# hold one comma-separated value per run, and the status columns the
# largest exit status of any run. A row is "ok" only if every run exited
# 0, every compressed output is identical, and every round trip is exact;
# otherwise the last column says what went wrong.
#
# ACZ_THREADS=N runs acz with N worker threads (xz and zstd stay single
# threaded, since their output depends on the thread count). acz's bytes do
# not change; its time columns are then CPU seconds over all threads. The
# decompressed output is compared as it is produced, never stored.
# ACZ_CODEC_CPUS="codec=cores ..." runs the codecs concurrently on one
# decompressed copy, each pinned to its own cores, and gives acz one thread
# per core (see the end).
#
# Columns:
#   host path_id src_bytes src_sha256 unpack unpack_status complete
#   content_bytes content_sha256 codec packed
#   enc_user enc_sys enc_wall enc_status dec_user dec_sys dec_wall dec_status cmp
set -u -o pipefail
src="$1"; mode="${2:-plain}"
A="${ACZ:-$HOME/acz-bench/acz}"
S="${ACZ_SCRATCH:-$HOME/acz-bench/tmp}"
CPU="${ACZ_CPU:-}"            # optional cores to pin to (taskset list, e.g. 7 or 5-8)
PREFIX="${ACZ_PREFIX:-}"      # optional: measure only this many bytes
REPS="${ACZ_REPS:-3}"
THREADS="${ACZ_THREADS:-1}"   # acz threads; acz's output is the same for any count
read -r -a codecs <<< "${CODECS:-acz4 acz3 xz9 zstd19}"
mkdir -p "$S"
T=$(mktemp -d "$S/m.XXXXXX")
trap 'rm -rf "$T"' EXIT
host=$(hostname -s)
pid=$(printf %s "$src" | sha256sum | cut -c1-16)
pin=(); [ -n "$CPU" ] && pin=(taskset -c "$CPU")
emit() { local IFS=$'\t'; echo "$*"; }
srcb=-; srch=-; umode="$mode"; ustatus=-; complete=-; cb=-; ch=-

# One row per codec recording why nothing was measured, then stop.
fail() {
  for c in "${codecs[@]}"; do
    emit "$host" "$pid" "$srcb" "$srch" "$umode" "$ustatus" "$complete" "$cb" "$ch" "$c" \
         - - - - - - - - - "$1"
  done
  exit 0
}

srcb=$(stat -c %s -- "$src") || fail ERR_STAT
srch=$(sha256sum < "$src" | cut -c1-64) || fail ERR_READ
in="$src"; complete=yes
if [ "$mode" = unpack ]; then
  case "$src" in
    *.bz2) dec="bzip2 -dc";; *.zst) dec="zstd -dcq";; *.gz) dec="gzip -dc";; *.xz) dec="xz -dc";;
    *.zip|*.ZIP) dec="unzip -p";;
    *) dec=;;
  esac
  cp -- "$src" "$T/src" || fail ERR_COPY
  if [ -z "$dec" ]; then
    mv "$T/src" "$T/raw"; umode=copy; ustatus=0
    if [ -n "$PREFIX" ]; then
      umode="copy-prefix:$PREFIX"; complete=no; truncate -s "<$PREFIX" "$T/raw"
    fi
  elif [ -z "$PREFIX" ]; then
    $dec "$T/src" > "$T/raw" 2> "$T/unpack.err"; ustatus=$?
    rm -f "$T/src"
    [ "$ustatus" -eq 0 ] || { complete=no; fail ERR_UNPACK; }
  else
    umode="prefix:$PREFIX"; complete=no
    $dec "$T/src" 2> "$T/unpack.err" | head -c "$PREFIX" > "$T/raw"
    st=("${PIPESTATUS[@]}"); rm -f "$T/src"
    # The decompressor is cut off by a full prefix (141 is SIGPIPE).
    if [ "$(stat -c %s "$T/raw")" -eq "$PREFIX" ] && { [ "${st[0]}" -eq 0 ] || [ "${st[0]}" -eq 141 ]; }; then
      ustatus=0
    elif [ "${st[0]}" -eq 0 ]; then
      ustatus=0; umode="prefix:$PREFIX(short)"
    else
      ustatus="${st[0]}"; fail ERR_UNPACK
    fi
  fi
  in="$T/raw"
fi
cb=$(stat -c %s -- "$in"); ch=$(sha256sum < "$in" | cut -c1-64)
[ "$cb" -gt 0 ] || fail EMPTY

enc() { case $1 in
  acz4) echo "$A -4 -T${CODEC_THREADS:-$THREADS} -c";; acz3) echo "$A -3 -T${CODEC_THREADS:-$THREADS} -c";;
  xz9) echo "xz -9 -T1 -c";; zstd19) echo "zstd -19 -T1 -q -c";; esac; }
dec() { case $1 in
  acz*) echo "$A -d -T${CODEC_THREADS:-$THREADS} -c";; xz9) echo "xz -d -T1 -c";; zstd19) echo "zstd -d -q -c";; esac; }
# Measure codec $1 pinned by the taskset arguments that follow (none: not
# pinned). Its files are named after it, so codecs can run concurrently.
run_codec() {
  local c=$1; shift
  local pin=("$@") es=0 ds=0 ok=ok eu= esys= ew= du= dsys= dw= r s1 s2 a b w st packed
  local q="$T/$c.q" p="$T/$c.p" te="$T/$c.te" td="$T/$c.td"
  [ -n "$(enc "$c")" ] || { emit "$host" "$pid" "$srcb" "$srch" "$umode" "$ustatus" "$complete" "$cb" "$ch" "$c" - - - - - - - - - UNKNOWN_CODEC; return; }
  for ((r = 1; r <= REPS; r++)); do
    /usr/bin/time -f "%U %S %e" -o "$te" "${pin[@]}" $(enc "$c") < "$in" > "$q"; s1=$?
    if [ "$r" -eq 1 ]; then mv "$q" "$p"; elif ! cmp -s "$p" "$q"; then ok=NONDETERMINISTIC; fi
    /usr/bin/time -f "%U %S %e" -o "$td" "${pin[@]}" $(dec "$c") < "$p" | cmp -s "$in" -
    st=("${PIPESTATUS[@]}"); s2=${st[0]}
    [ "${st[1]}" -eq 0 ] || { [ "$ok" = ok ] && ok=DIFF; }
    [ "$s1" -gt "$es" ] && es=$s1; [ "$s2" -gt "$ds" ] && ds=$s2
    read -r a b w < <(tail -1 "$te"); eu+="${eu:+,}$a"; esys+="${esys:+,}$b"; ew+="${ew:+,}$w"
    read -r a b w < <(tail -1 "$td"); du+="${du:+,}$a"; dsys+="${dsys:+,}$b"; dw+="${dw:+,}$w"
    rm -f "$q"
  done
  packed=$(stat -c %s "$p")
  [ "$es" -eq 0 ] && [ "$ds" -eq 0 ] || ok="FAIL($ok)"
  emit "$host" "$pid" "$srcb" "$srch" "$umode" "$ustatus" "$complete" "$cb" "$ch" "$c" "$packed" \
       "$eu" "$esys" "$ew" "$es" "$du" "$dsys" "$dw" "$ds" "$ok"
  rm -f "$p"
}

if [ -z "${ACZ_CODEC_CPUS:-}" ]; then
  for c in "${codecs[@]}"; do run_codec "$c" "${pin[@]}"; done
else
  # ACZ_CODEC_CPUS="acz4=15-17 xz9=6 ...": every codec runs at once on the
  # one decompressed copy, each pinned to its own cores; rows are printed
  # in codec order once all have finished.
  declare -A cores=()
  for kv in $ACZ_CODEC_CPUS; do cores[${kv%%=*}]=${kv#*=}; done
  # An acz codec gets one thread per core it is given.
  ncores() { local n=0 r; for r in ${1//,/ }; do
    if [[ $r == *-* ]]; then n=$((n + ${r#*-} - ${r%-*} + 1)); else n=$((n + 1)); fi; done; echo "$n"; }
  for c in "${codecs[@]}"; do
    if [ -n "${cores[$c]:-}" ]; then
      CODEC_THREADS=$(ncores "${cores[$c]}") run_codec "$c" taskset -c "${cores[$c]}"
    else
      run_codec "$c"
    fi > "$T/$c.row" &
  done
  wait
  for c in "${codecs[@]}"; do cat "$T/$c.row"; done
fi
