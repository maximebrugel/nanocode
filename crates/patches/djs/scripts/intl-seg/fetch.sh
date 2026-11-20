#!/usr/bin/env bash
# Fetch ICU 78.3's dictionary sources (node 22.23.5's ICU) that gendict.py reads.
#   scripts/intl-seg/fetch.sh DIR
set -euo pipefail
D=${1:?dir}; mkdir -p "$D"
B=https://raw.githubusercontent.com/unicode-org/icu/release-78.3/icu4c/source/data/brkitr/dictionaries
for f in cjdict thaidict laodict burmesedict khmerdict; do curl -sfL -o "$D/$f.txt" "$B/$f.txt"; done
