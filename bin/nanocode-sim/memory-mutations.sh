#!/usr/bin/env bash
set -euo pipefail
root=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
mkdir -p "$root/output"
work=$(mktemp -d "$root/output/memory-mutations.XXXXXX")
git clone --quiet --shared "$root" "$work/core"
cd "$work/core"
export SHIPPY_REMOTE_BUILD=0
export CARGO_TARGET_DIR=${MEMORY_MUTATIONS_TARGET:-$work/target}
check=(--memory --memory-gate-retained --only claude/paged --turns 30)
prefix=(--memory --only codex/reload --turns 10)
cargo run --release --offline --locked -p nanocode-sim -- "${check[@]}" > "$work/baseline.log" 2>&1
printf 'baseline_exit=0\n' > "$work/exits.txt"
cargo run --release --offline --locked -p nanocode-sim -- "${prefix[@]}" > "$work/prefix-baseline.log" 2>&1
printf 'prefix_baseline_exit=0\n' >> "$work/exits.txt"
python3 - <<'PY'
from pathlib import Path
p = Path('crates/backends/claude/src/agent/durable.rs')
s = p.read_text()
old = '            paging.committed();\n'
assert s.count(old) == 1
p.write_text(s.replace(old, '            let _ = &mut paging;\n'))
PY
set +e
cargo run --release --offline --locked -p nanocode-sim -- "${check[@]}" > "$work/mutant.log" 2>&1
status=$?
printf 'mutant_exit=%s\n' "$status" >> "$work/exits.txt"
set -e
if [ "$status" -eq 0 ]; then
  printf 'FAIL: memory mutation escaped: %s\n' "$work"
  exit 1
fi
if ! grep -q 'MEMORY RETAINED' "$work/mutant.log"; then
  printf 'FAIL: mutation did not reach the memory invariant: %s\n' "$work"
  exit 1
fi
printf 'PASS: history kept beside its committed sequence log caught; evidence: %s\n' "$work"
git checkout --quiet -- .
python3 - <<'PY'
from pathlib import Path
p = Path('crates/backends/codex/client/src/session/compaction.rs')
s = p.read_text()
old = 'let kept = max_bytes - widest.len();'
assert s.count(old) == 1
p.write_text(s.replace(old, 'let kept = max_bytes;'))
PY
set +e
cargo run --release --offline --locked -p nanocode-sim -- "${prefix[@]}" > "$work/prefix-mutant.log" 2>&1
status=$?
printf 'prefix_mutant_exit=%s\n' "$status" >> "$work/exits.txt"
set -e
if [ "$status" -eq 0 ]; then
  printf 'FAIL: prefix mutation escaped: %s\n' "$work"
  exit 1
fi
if ! grep -q 'MEMORY.*PREFIX' "$work/prefix-mutant.log"; then
  printf 'FAIL: prefix mutation did not reach the prefix check: %s\n' "$work"
  exit 1
fi
printf 'PASS: re-truncated tool output on reload caught; evidence: %s\n' "$work"
