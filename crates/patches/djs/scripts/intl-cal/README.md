# scripts/intl-cal

Intl.DateTimeFormat's non-gregorian calendars, derived from reference node v22.23.5 (ICU 78.3, `PARITY_NODE_DIR`).
The Rust side is `crates/djs-core/src/builtins/intl/date/calendar.rs` (arithmetic, option resolution, field
formatting) and the generated `calendar_data.rs` (names, patterns, lunisolar and hijri tables).

Calendars: buddhist, roc, japanese, persian, islamic-civil, islamic-tbla, islamic-umalqura, islamic, islamic-rgsa,
hebrew, indian, coptic, ethiopic, ethioaa, chinese, dangi, iso8601 (aliases islamicc, ethiopic-amete-alem).
Each has en-US data plus its native locale where djs has that DateLocale (zh-TW roc, ja japanese, ar islamic*/coptic,
he hebrew, hi indian, zh chinese, ko dangi). A calendar a locale has no data for formats gregorian and resolves
`calendar` to gregory, so the two always agree.

| file | what |
| --- | --- |
| cal.js | the calendar model calendar.rs mirrors: fields(days) per calendar |
| arith.js CAL... | checks cal.js against node on every day 1900-2100 (era, year, month, day, related year, cyclic name) |
| derive.js | tables.json: chinese/dangi lunisolar years and umalqura/islamic hijri years, 1600-2400, from node |
| cinfer.js TAG | c-TAG.json: names and the 864 date / date+zone patterns, dateStyles, date-time glue, and time tables where a calendar's times differ (zh-TW roc, zh chinese, ko dangi, iso8601) |
| numsys.js | the numberings calendar patterns override fields with (Hebrew letters, Han digits, Chinese day names, 元年) |
| gen.sh OUT TAG... | runs cinfer.js four at a time |
| emit.js DIR | calendar_data.rs from DIR's c-*.json and tables.json |
| diff.js, check.sh DJS CAL | the differential: ~1900-3750 cases per calendar (en-US + native, 75 option sets, random dates 1900-2100, toLocale*String, calendar option) under node and djs |
| suite.py | the tempoxyz/nanocode-rt parity suite intl-dtf-cal.jsonl |

Pattern extensions: `r` related gregorian year, `U` cyclic year name, and U+E001..U+E004 before a numeric field
for hebr / hanidec / hanidays / jpanyear numbering.

Regenerate: `node derive.js && ./gen.sh ~/cal/data $(TAGS) && node emit.js ~/cal/data > ../../crates/djs-core/src/builtins/intl/date/calendar_data.rs`
with TAGS the en-US-u-ca-CAL and native tags above.

