#!/bin/bash
# check.sh DJS CAL [TAGS...]: runs diff.js under node (PARITY_NODE_DIR) and under djs, prints the mismatch count and the first few
here=$(dirname "$0"); djs=$1; shift
"$PARITY_NODE_DIR/node" "$here/diff.js" "$@" 2>/dev/null > /tmp/cal-node-$1.txt
{ printf 'const CAL_ARGS = %s;\n' "$(python3 -c 'import json,sys; print(json.dumps(sys.argv[1:]))' "$@")"; cat "$here/diff.js"; } > /tmp/cal-src-$1.js
python3 ~/rng2/s/wsrun.py "$djs" /tmp/cal-src-$1.js > /tmp/cal-djs-$1.txt 2>&1 || true
python3 - "$1" <<'EOF'
import sys
c=sys.argv[1]
a=open(f'/tmp/cal-node-{c}.txt').read().strip().split('\n'); b=open(f'/tmp/cal-djs-{c}.txt').read().strip().split('\n')
bad=[(x,y) for x,y in zip(a,b) if x!=y]
print(c,'cases',len(a),'djs',len(b),'mismatch',len(bad)+abs(len(a)-len(b)))
for x,y in bad[:int(__import__('os').environ.get('SHOW','6'))]: print('  node',x[:500]); print('  djs ',y[:500])
EOF

