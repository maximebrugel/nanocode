#!/usr/bin/env bash
# The diff loop on a Hand: regenerate builtins, base.img and the runjs example, then diff
# against node 22.   scripts/intl-loc/loop.sh CLDR OUTDIR [group...]
# Env: NODE (node 22), CARGO_TARGET_DIR. Prints base.img's size.
set -euo pipefail
cd "$(dirname "$0")/../.."
CLDR=${1:?cldr dir}; OUT=${2:?out dir}; shift 2
scripts/bgen.sh >/dev/null
scripts/base.sh 2>&1 | grep bytes
cargo build --release -q -p djs-embed --example runjs 2>&1 | grep -E '^(error|warning)' -A8 | head -60 || true
python3 scripts/intl-loc/diff.py "${CARGO_TARGET_DIR:-target}/release/examples/runjs" "$OUT" "$CLDR" "$@"
