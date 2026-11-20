# Intl.DateTimeFormat locale data

`builtins/intl/date/locales.rs` is generated from Node's ICU by probing Intl.DateTimeFormat:

    L="de fr es ja zh pt-BR ko it ru nl pl tr ar hi id vi zh-TW en-GB en-IN es-MX fr-CA"
    for l in $L; do node infer.js $l > t-$l.json && node gen.js $l && node zn.js $l > z-$l.json; done
    node check.js de        # composes patterns like djs and compares with node on random option sets
    node emit.js . > ../../crates/djs-core/src/builtins/intl/date/locales.rs
    node alias.js ar ar-EG  # checks that an alias formats exactly like its locale

emit.js lists the locales (LOCS) and the tags each answers to (ALIASES). A tag that isn't listed
uses its bare language's data (it-CH formats as it), and zh tags with Hant, TW, HK or MO use zh-TW.
Arabic regions whose node default is numberingSystem arab (ar-EG, ar-SA and others) format with
ar's data and Arabic-Indic digits, which date.rs substitutes after formatting.

A glue (glue.js) joins a date pattern and a time pattern: either the literal between them, or a
template with {1} for the date and {0} for the time when the time comes first or the pair is
wrapped (hi, vi).

To keep the binary small, the four pattern tables of every locale are cut into rows of 16 cells
stored once in DTF_ROWS, and the pattern and zone-name pools are each one text with end offsets.

zones.tsv is the zone list of data.rs's ZONES (name, POSIX rule), in order.
intl-dtf.jsonl holds parity cases (copy to the parity suites as suites/js/intl-dtf.jsonl);
mkcases.py FILE writes them and matrix.py compares djs with node over options x zones x dates.

range.js LOC (after infer.js LOC > t-LOC.json) writes r-LOC.json: node's formatRange interval
patterns for every date key (era, year, month and day levels), every time key without seconds
(am/pm, hour and minute levels, plain and with each timeZoneName), and the date and time styles,
each cut where the end date starts and checked against formatRangeToParts on three date pairs.
Under node v22.23.5 all 22 locales infer with no mismatches except it (96 era-level keys).

formatRange and formatRangeToParts (intl/date/range.rs) follow ICU's DateIntervalFormat as V8
drives it: the greatest differing field (era, year, month, day, am/pm, hour, minute, second,
millisecond) picks an interval pattern; ICU's fallback formats both dates in one pattern joined by
the range separator; when no field the pattern shows differs, the start is formatted alone. The
tables it reads (intl/date/range_data.rs, about 11k deduplicated pattern pairs in rows of 16 like
DTF_ROWS) are written by rangeemit.js from, per locale:

- r-LOC.json (range.js): interval patterns by date key, time key (with era and day levels) and style;
- tp-LOC.json (tpat.js, then dpat.js, then tpatf.js): ICU's own time pattern per time key and zone
  kind, the date pattern it joins to a time range and the medium glue, and the time and date
  patterns its fallbacks use (they differ from format's: de HH, normalized date skeletons).

rangemodel.js is the same algorithm in JS and rdiff.js checks it against node. rmatrix.py (cases
from rcases.js) is the Rust differential through the parity harness's ws binary:
  python3 rmatrix.py target/release/ws OUT en-US,de,fr,ja,ar,vi,pt-BR,zh-TW 300
24000 cases (option sets x pairs at every level, 4 zones) match node 94.7%. Known gaps: date with
time and with zone names (ICU skeleton adjustments), weekday with hour, ru/vi/zh time-only
intervals; mkcases.py's RANGE_TODO lists the parity cases they fail.
