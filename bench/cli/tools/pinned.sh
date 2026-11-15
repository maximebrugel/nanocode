#!/usr/bin/env bash
# Pinned memory rows for one nanocode build: tools/pinned.sh <nanocode front, or its directory> [runs] [parts]
# parts: comma list of suite.mjs parts, default tools,oneshot (10-step direct and Code Mode tool turns, one-shot); runs default 10.
# Runs suite.mjs for the nc lane under taskset -c PINNED_CPUS (default 4), setarch -R (PINNED_ASLR=1 keeps ASLR),
# tools/localfs.sh and tools/netguard.sh, with PINNED_WARMUP (default 1) extra rounds first that tools/pinned.py drops.
# PINNED_LOCK_DIR names a directory with a buildq.sh queue (.buildq) and slot lock files (.build-slot-N): the run takes a
# ticket and holds every slot in PINNED_SLOTS (default "1 2 3 4") so no queued build runs beside it. Do not also wrap it in
# that queue. Compare ru_maxrss medians only between runs with the same PINNED_CPUS.
# Writes info.txt, suite.jsonl, run.log and summary.txt to PINNED_OUT (default work/pinned/<time>-<label>).
set -euo pipefail
[ $# -ge 1 ] || { echo "usage: pinned.sh <nanocode> [runs] [parts]" >&2; exit 2; }
bin=$(realpath "$1"); [ -d "$bin" ] && bin=$bin/nanocode
[ -x "$bin" ] || { echo "pinned.sh: $bin is not executable" >&2; exit 2; }
runs=${2:-10}; parts=${3:-tools,oneshot}
harness=$(cd "$(dirname "$0")/.." && pwd)
cpus=${PINNED_CPUS:-4}
warm=${PINNED_WARMUP:-1}
label=${PINNED_LABEL:-$(basename "$(dirname "$bin")")}
out=${PINNED_OUT:-$harness/work/pinned/$(date -u +%Y%m%dT%H%M%S)-$label}
mkdir -p "$out"
total=$((runs + warm))
aslr=(setarch "$(uname -m)" -R); [ "${PINNED_ASLR:-0}" = 1 ] && aslr=()
cd "$harness"
inner=(env NC_BIN="$bin" BENCH_RESULTS="$out" BENCH_WORK="$out/work" BENCH_WORKSPACE="$out/ws" taskset -c "$cpus" "${aslr[@]}" tools/localfs.sh tools/netguard.sh node harness/suite.mjs --parts="$parts" --only=nc --rounds="$total" --toolrounds="$total" --steps=10)
{ echo "binary $bin sha256 $(sha256sum "$bin" | cut -c1-16) agent $(sha256sum "$(dirname "$bin")/nanocode-agent" 2>/dev/null | cut -c1-16)"; echo "cpus $cpus aslr ${PINNED_ASLR:-0} warmup $warm runs $runs harness $(git rev-parse --short HEAD 2>/dev/null || echo copy) kernel $(uname -r)"; } > "$out/info.txt"
if [ -n "${PINNED_LOCK_DIR:-}" ]; then
  q=$PINNED_LOCK_DIR/.buildq
  mkdir -p "$q"
  t=$( { flock 8; n=$(( $(cat "$q/next" 2>/dev/null || echo 0) + 1 )); echo $n > "$q/next"; echo $n; } 8>"$q/lock" )
  touch "$q/w.$t"
  ( while [ -e "$q/w.$t" ]; do touch "$q/w.$t" 2>/dev/null; sleep 30; done ) &
  trap 'rm -f "$q/w.$t"' EXIT
  locks=()
  for s in ${PINNED_SLOTS:-1 2 3 4}; do locks+=(flock -o "$PINNED_LOCK_DIR/.build-slot-$s"); done
  "${locks[@]}" sh -c 'rm -f "$1"; echo "slots held, load $(cut -d" " -f1-3 /proc/loadavg)" >> "$0/info.txt"; shift; exec "$@"' "$out" "$q/w.$t" "${inner[@]}" > "$out/run.log" 2>&1
else
  "${inner[@]}" > "$out/run.log" 2>&1
fi
python3 tools/pinned.py "$out/suite.jsonl" "$warm" | tee "$out/summary.txt"
rm -rf "$out/work" "$out/ws"
