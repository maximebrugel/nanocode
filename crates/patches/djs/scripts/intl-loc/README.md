# intl-loc

Intl.Locale, getCanonicalLocales, supportedValuesOf, DisplayNames and Segmenter, matched
against node 22 (ICU 78, CLDR 48, Unicode 17).

- `fetch.sh DIR`: the CLDR 48 JSON and Unicode 17 UCD files the generators read.
- `gen.js DIR > ../../crates/djs-core/src/builtins/intl/locdata.rs`: likely subtags, aliases,
  extension types, locale info and supportedValuesOf, every value taken from node.
- `dngen.js DIR > .../dndata.rs`: DisplayNames names for en de fr es it pt nl ja zh ko ru
  (plus zh-Hant, pt-PT, en-GB, es-419, es-MX, fr-CA, de-AT overlays).
- `segdata.js DIR > .../segdata.rs`: grapheme, word and sentence break properties.
- `matrix.js DIR [group...]`: the differential cases (groups canon locale maxmin info svo dn
  dnx seg); `diff.py RUNJS OUT DIR [group...]` runs them under node and runjs and buckets
  the mismatches; `loop.sh DIR OUT [group...]` regenerates builtins and base.img, builds
  runjs and diffs (run all generators under node 22).
- `intl-locale.jsonl`: ~50 pinned cases with node's output.
