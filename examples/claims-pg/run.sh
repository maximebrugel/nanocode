#!/usr/bin/env bash
# examples/claims-pg/run.sh: start a private Postgres 16 under output/claims-pg/pg (port 5544) if it
# is not running, then run claims-pg once per argument set, each on a fresh database.
#   examples/claims-pg/run.sh "parts=16 claimers=64" "parts=64 claimers=64"
# Env: PG_BIN (default /usr/lib/postgresql/16/bin), PG_PORT (default 5544), PG_DATA (default
# output/claims-pg/pg), PG_CPUS and BENCH_CPUS (taskset lists for the
# server and the client; default: no pinning), CLAIMS_BIN (default target/release/claims-pg),
# PLAN_CACHE_MODE (set on each run's database, such as force_generic_plan).
# An argument set with nodes=N (N > 1) starts N processes, node=0..N-1, on the one database, each
# owning its range of the board partitions, all held to one start_ms (START_LEAD_MS after launch,
# default 4000) so their claim phases overlap; NODE_CPUS (taskset lists split by spaces, one per
# node) pins them in place of BENCH_CPUS. It prints a TOTAL line: claims over the span from the
# first node's start to the last node's end, the slowest node's p99 and Postgres transactions per
# claim over all nodes. TAKEOVER_MS=T adds adopt=0 adopt_ms=start+T to the last node, which then
# takes node 0's partitions; TAKEOVER_KILL=1 (default) kills node 0 at that time first, as a crash;
# 0 leaves it running, so its writes after the takeover are fenced.
set -euo pipefail
root=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
cd "$root"
pg_bin=${PG_BIN:-/usr/lib/postgresql/16/bin}
data=${PG_DATA:-output/claims-pg/pg}
port=${PG_PORT:-5544}
bin=${CLAIMS_BIN:-target/release/claims-pg}
pin() { local cpus=$1; shift; if [ -n "$cpus" ]; then taskset -c "$cpus" "$@"; else "$@"; fi; }
if [ ! -f "$data/PG_VERSION" ]; then
  mkdir -p "$data"
  "$pg_bin/initdb" -D "$data" -U nc --auth=trust >/dev/null
  cat >>"$data/postgresql.conf" <<CONF
port = $port
listen_addresses = '127.0.0.1'
unix_socket_directories = ''
max_connections = 400
shared_buffers = 4GB
wal_buffers = 64MB
max_wal_size = 16GB
checkpoint_timeout = 30min
synchronous_commit = on
fsync = on
CONF
fi
if ! "$pg_bin/pg_isready" -h 127.0.0.1 -p $port -q; then
  pin "${PG_CPUS:-}" "$pg_bin/pg_ctl" -D "$data" -l "$data/../server.log" -w start >/dev/null
fi
# multi URL NODES ARGS: every node of one argument set, then their TOTAL line.
multi() {
  local url=$1 nodes=$2 args=$3 start out pids=() i
  start=$(( $(date +%s%3N) + ${START_LEAD_MS:-4000} ))
  out=$(mktemp -d)
  read -r -a cpus <<<"${NODE_CPUS:-}"
  for ((i = 0; i < nodes; i++)); do
    local extra=""
    if [ -n "${TAKEOVER_MS:-}" ] && [ "$i" -eq $((nodes - 1)) ]; then
      extra="adopt=0 adopt_ms=$((start + TAKEOVER_MS))"
    fi
    # shellcheck disable=SC2086
    pin "${cpus[$i]:-${BENCH_CPUS:-}}" "$bin" "$url" $args node=$i start_ms=$start $extra \
      >"$out/$i" 2>&1 &
    pids+=($!)
  done
  if [ -n "${TAKEOVER_MS:-}" ] && [ "${TAKEOVER_KILL:-1}" = 1 ]; then
    while [ "$(date +%s%3N)" -lt $((start + TAKEOVER_MS)) ]; do sleep 0.01; done
    kill -9 "${pids[0]}" 2>/dev/null || true
    echo "KILLED node=0 at_ms=$(date +%s%3N)"
  fi
  for pid in "${pids[@]}"; do wait "$pid" || true; done
  for ((i = 0; i < nodes; i++)); do cat "$out/$i"; done
  cat "$out"/* | awk '/^METRIC/ {
      for (f = 2; f <= NF; f++) { split($f, kv, "="); m[kv[1]] = kv[2] }
      n++; claims += m["claims"]; trips += m["trips"]
      if (n == 1 || m["start_unix_ms"] < first) first = m["start_unix_ms"]
      if (m["end_unix_ms"] > last) last = m["end_unix_ms"]
      if (m["p99_ms"] > p99) p99 = m["p99_ms"]
      if (m["late_ms"] > late) late = m["late_ms"]
      per = per sep m["claims_per_s"]; sep = ","
    } END {
      if (n) printf "TOTAL nodes=%d claims=%d claims_per_s=%.0f per_node=%s max_p99_ms=%.2f pg_txns_per_claim=%.2f span_s=%.2f max_late_ms=%d\n",
        n, claims, claims / ((last - first) / 1000), per, p99, trips / (claims > 0 ? claims : 1), (last - first) / 1000, late
    }'
  rm -rf "$out"
}
n=0
for args in "$@"; do
  n=$((n + 1))
  db="claims_$$_$n"
  "$pg_bin/createdb" -h 127.0.0.1 -p $port -U nc "$db"
  if [ -n "${PLAN_CACHE_MODE:-}" ]; then
    "$pg_bin/psql" -h 127.0.0.1 -p $port -U nc -X -q -d "$db" \
      -c "ALTER DATABASE $db SET plan_cache_mode = $PLAN_CACHE_MODE"
  fi
  nodes=$(sed -n 's/.*\bnodes=\([0-9]*\).*/\1/p' <<<"$args")
  if [ "${nodes:-1}" -gt 1 ]; then
    multi "postgres://nc@127.0.0.1:$port/$db" "$nodes" "$args"
  else
    # shellcheck disable=SC2086
    pin "${BENCH_CPUS:-}" "$bin" "postgres://nc@127.0.0.1:$port/$db" $args || true
  fi
  "$pg_bin/dropdb" -h 127.0.0.1 -p $port -U nc "$db"
done
