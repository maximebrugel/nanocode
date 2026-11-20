#!/usr/bin/env bash
# Fetch the CLDR 48 and Unicode 17 sources gen.js and segdata.js read (node 22's ICU 78 versions).
#   scripts/intl-loc/fetch.sh DIR
set -euo pipefail
D=${1:?dir}; mkdir -p "$D/bcp47" "$D/ucd"
C=https://raw.githubusercontent.com/unicode-org/cldr-json/48.0.0/cldr-json
for f in likelySubtags aliases territoryInfo; do curl -sfL -o "$D/$f.json" "$C/cldr-core/supplemental/$f.json"; done
curl -sfL -o "$D/availableLocales.json" "$C/cldr-core/availableLocales.json"
for f in calendar collation currency measure number timezone variant segmentation transform; do curl -sfL -o "$D/bcp47/$f.json" "$C/cldr-bcp47/bcp47/$f.json"; done
U=https://www.unicode.org/Public/17.0.0/ucd
for f in auxiliary/GraphemeBreakProperty.txt auxiliary/WordBreakProperty.txt auxiliary/SentenceBreakProperty.txt emoji/emoji-data.txt DerivedCoreProperties.txt auxiliary/GraphemeBreakTest.txt auxiliary/WordBreakTest.txt auxiliary/SentenceBreakTest.txt Scripts.txt LineBreak.txt; do curl -sfL -o "$D/ucd/$(basename $f)" "$U/$f"; done
C=https://raw.githubusercontent.com/unicode-org/cldr-json/48.0.0/cldr-json; mkdir -p "$D/names"; for f in languages territories scripts variants; do curl -sfL -o "$D/names/$f.json" "$C/cldr-localenames-full/main/en/$f.json"; done; curl -sfL -o "$D/names/currencies.json" "$C/cldr-numbers-full/main/en/currencies.json"
