# claims-pg

Board claims on a real Postgres 16+: see the header of `src/main.rs` for every option and
`run.sh` for the private Postgres it starts.

## One board partition (parts=1) tops out near 13-17k claims/s: use parts >= 4

At parts=1 there is one `shards::Committer` and one log partition, so exactly one Postgres
commit is in flight and claims/s = rows per commit / commit cycle. Raising rows per commit does
not help much, because the cycle grows linearly with its rows rather than staying fixed:

- The cap on rows per commit is `batch` (the Committer's group, 256 by default) and then
  `max_rows` (512). At claimers=64 prefetch=1 a commit carries about 85 of the 128 possible rows
  (claimers straggle: each waits on its own previous commit); with prefetch >= 4 or claimers=256
  every commit is full at `batch`.
- The append INSERT is linear, not superlinear: EXPLAIN ANALYZE on a fresh table takes 0.55, 2.7,
  5.5 and 11 ms at 64, 512, 1024 and 2048 rows (about 5.4 us a row: the clash anti-join about 1.5
  ms, the insert with its two indexes about 3 ms and 380 KB of WAL per 1024 rows).
- Under load the whole cycle costs about 35-40 us a row, split roughly evenly between the
  Postgres transaction (plog_commit: 1.5 ms at 64 rows, 15-20 ms at 1024) and the client's serial
  fold around it (group, judge, apply, verdicts back to the claimers). So parts=1 converges on
  about 25-28k board writes/s, i.e. 13-14k claims/s, whatever `batch` and `max_rows` are.

Measured on a shared 24-core box (2 runs each, tasks=30000, claims/s):

| parts=1 | batch=max_rows=64 | 256 | 1024 |
| --- | --- | --- | --- |
| claimers=64 prefetch=1 | 10.1k / 11.0k | 10.7k / 9.9k | 10.3k / 9.7k |
| claimers=64 prefetch=4 | 10.2k / 9.8k | 12.4k / 11.1k | 12.4k / 12.7k |
| claimers=64 prefetch=8 | 10.0k / 9.9k | 13.3k / 12.2k | 15.5k / 13.1k |
| claimers=256 prefetch=1 | 10.6k / 10.6k | 13.4k / 16.0k | 13.1k / 14.9k |
| claimers=256 prefetch=4 | 8.7k / 10.4k | 11.9k / 12.7k | 12.9k / 13.7k |
| claimers=256 prefetch=8 | 10.1k / 10.3k | 12.1k / 13.3k | 13.5k / 13.7k |

parts=16 claimers=64 on the same box and runs: 28.2k / 25.7k.

Reaching 50k claims/s on one partition would need under 10 us a row for Postgres and client
together. Partitions scale it instead: run parts >= 4 (pyramid2's PYRAMID2_PARTITIONS) when one
deployment needs more than about 10k claims/s.

`writes_per_append` in the METRIC line averages over the whole run, including the phase that
posts the tasks, so it understates the claim phase's rows per commit once `batch` is above
256; log the INSERTs (auto_explain) for the real number.

