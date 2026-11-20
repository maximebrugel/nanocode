#!/bin/bash
# gen.sh OUTDIR TAG...: c-TAG.json for each tag, four at a time, with the parity node (PARITY_NODE_DIR)
out=$1; shift; mkdir -p "$out"
printf '%s\n' "$@" | OUT=$out xargs -P 4 -I{} sh -c '"$PARITY_NODE_DIR/node" "$(dirname "$0")/cinfer.js" {} 2>&1 | grep -v -i warn' "$0"
