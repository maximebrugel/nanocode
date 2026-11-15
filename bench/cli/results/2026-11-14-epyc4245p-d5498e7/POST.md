# Seven agent CLIs, measured, 2026-11-14

*Draft for nanocode.xyz/experiments. Not published. Generated from the results' JSONL by `python3 harness/post.py --out results/2026-11-14-epyc4245p-d5498e7/POST.md --since results/2026-11-14-nc-since/since.json results/2026-11-14-epyc4245p-d5498e7 results/2026-11-14-fra2-epyc9254 results/2026-11-13-epyc4245p results/2026-11-13-epyc4245p-b results/2026-11-13-epyc4245p-c results/2026-11-14-dev-georgios-store14600` (run from bench/cli); edit harness/post.py, not this file.*

Seven terminal coding agents ran the same scripted work: nc (nanocode), nc-lite (the same SDK cut down to what fx ships), Vercel fx, Pi, OpenCode, Claude Code and Codex CLI. No lane talks to a real model: each points at a local fixture that streams a fixed reply, runs with its own empty HOME, inside a network namespace where only loopback works. The lanes take turns in every round. Every number is a median with the sample count in parentheses; CPU and memory cover the CLI's whole process tree. Lower is better in every row except the KB-printed context row.

## Where each number comes from

Each row below names its source. A row takes all of its lanes from one source, so numbers in a row are always from the same machine, run and nanocode commit.

| Source | Machine | When (UTC) | Load average (fleet rows aside, which load the box themselves) | nanocode commit | Data |
|---|---|---|---|---|---|
| A | AMD EPYC 4245P 6-Core Processor, 12 threads, 46 GB, Ubuntu 24.04.5 LTS, kernel 6.8.0-149-generic, governor powersave | 2026-11-14 22:00 to 22:47 | 0.0 to 1.2 | d5498e784 | [2026-11-14-epyc4245p-d5498e7](./) |
| B | AMD EPYC 9254 24-Core Processor, 48 threads, 377 GB, Ubuntu 24.04.5 LTS, kernel 7.0.0-43-generic, governor performance | 2026-11-14 06:50 to 08:21 | 0.1 to 2.2 | 41dcbc7c4 | [2026-11-14-fra2-epyc9254](../2026-11-14-fra2-epyc9254/) |
| C | AMD EPYC 4245P 6-Core Processor, 12 threads, 46 GB, Ubuntu 24.04.5 LTS, kernel 6.8.0-149-generic, governor powersave | 2026-11-13 11:52 to 12:52 | 0.0 to 1.0 | 4be51211a | [2026-11-13-epyc4245p](../2026-11-13-epyc4245p/) |
| D | AMD EPYC 4245P 6-Core Processor, 12 threads, 46 GB, Ubuntu 24.04.5 LTS, kernel 6.8.0-149-generic, governor powersave | 2026-11-13 16:15 to 17:46 | 0.0 to 1.8 | 223cb39ba | [2026-11-13-epyc4245p-b](../2026-11-13-epyc4245p-b/) |
| E | AMD EPYC 4244P 6-Core Processor, 12 threads, 62 GB, Ubuntu 24.04.5 LTS, kernel 6.8.0-149-generic, governor powersave | 2026-11-13 23:26 to 00:30 | 0.9 to 1.5 | de77f76e8 | [2026-11-13-epyc4245p-c](../2026-11-13-epyc4245p-c/) |
| F | AMD EPYC 4585PX 16-Core Processor, 32 threads, 62 GB, Ubuntu 24.04.5 LTS, kernel 6.8.0-149-generic, governor powersave, dev-georgios (shared development server); shared and loaded, so compare lanes within a row, not machines | 2026-11-14 04:24 to 04:37 | 75.1 to 221.9 | 62a0936e4 | [2026-11-14-dev-georgios-store14600](../2026-11-14-dev-georgios-store14600/) |

Versions, from each source's VERSIONS.md: nc 0.7.0; fx 0.0.16; nc-lite 0.7.0; Claude Code 2.1.323, 2.1.325; Codex 0.169.0; Pi 1.5.2; OpenCode 1.19.5. nc is built as releases are (scripts/build-cli.sh: dist profile, non-PIE, BOLT); the source table gives its commit.

## Starting up

| | nc | fx | nc-lite | Claude Code | Codex | Pi | OpenCode | Src |
|---|---:|---:|---:|---:|---:|---:|---:|---|
| First frame, ms | 1.19 (20) | 1.95 (20) | 0.896 (20) | n/a | n/a | n/a | n/a | A |
| CPU to the first frame, ms | 3.17 (20) | 4.26 (20) | 0.787 (20) | n/a | n/a | n/a | n/a | A |
| Memory 1 s after the first frame, RSS MB | 10.3 (20) | 8.81 (20) | 3.09 (20) | n/a | n/a | n/a | n/a | A |
| --version, ms | 2.63 (100) | 2.62 (100) | 2.62 (100) | n/a | n/a | n/a | n/a | A |
| --version, CPU ms | 0.224 (100) | 0.099 (100) | 0.255 (100) | n/a | n/a | n/a | n/a | A |
| --version, cold page cache, ms | 5.35 (10) | 5.07 (10) | 5.28 (10) | n/a | n/a | n/a | n/a | A |
| First frame, cold page cache, ms | 5.57 (10) | 9.35 (10) | 2.56 (10) | n/a | n/a | n/a | n/a | A |

## Using it

| | nc | fx | nc-lite | Claude Code | Codex | Pi | OpenCode | Src |
|---|---:|---:|---:|---:|---:|---:|---:|---|
| Keystroke echo p50, ms | 0.037 (10) | 0.279 (10) | 0.014 (10) | n/a | n/a | n/a | n/a | A |
| Keystroke echo, slowest of 26, ms | 0.082 (10) | 113 (10) | 0.052 (10) | n/a | n/a | n/a | n/a | A |
| Idle CPU, ms per s | -0.009 (10) | 0.609 (10) | -0.003 (10) | n/a | n/a | n/a | n/a | A |

## Headless

| | nc | fx | nc-lite | Claude Code | Codex | Pi | OpenCode | Src |
|---|---:|---:|---:|---:|---:|---:|---:|---|
| One-word reply, ms | 4.79 (20) | 22.8 (20) | 2.81 (20) | n/a | n/a | n/a | n/a | A |
| CPU for that reply, ms | 2.36 (20) | 1.59 (20) | 1.70 (20) | n/a | n/a | n/a | n/a | A |
| Tool turn (Code Mode), 50 shell steps, ms | 38.0 (10) | 1,059 (10) | 27.0 (10) | n/a | n/a | n/a | n/a | A |
| Tool turn (Code Mode), tool result to next request p50, ms | 0.339 (10) | 20.5 (10) | 0.302 (10) | n/a | n/a | n/a | n/a | A |
| Tool turn (Code Mode), 10 shell steps, CLI process peak RSS MB | 25.4 (10) | 9.25 (10) | 11.9 (10) | n/a | n/a | n/a | n/a | A |
| 32 runs at once, peak RSS MB each | 16.3 (10) | 7.74 (10) | 8.15 (9) | 227 (10) | 152 (10) | 125 (10) | 599 (10) | B |
| 32 runs at once, CPU ms each | 19.2 (10) | 19.1 (10) | 29.7 (9) | 793 (10) | 332 (10) | 700 (10) | 4,799 (10) | B |

## Streaming

| | nc | fx | nc-lite | Claude Code | Codex | Pi | OpenCode | Src |
|---|---:|---:|---:|---:|---:|---:|---:|---|
| Token on screen at 100 tok/s, p50 ms | 0.038 (10) | 200 (10) | 0.021 (10) | n/a | n/a | n/a | n/a | A |
| Token on screen at 1,000 tok/s, p50 ms | 5.03 (10) | 32.6 (10) | 0.015 (10) | n/a | n/a | n/a | n/a | A |
| Token on screen at 1,000 tok/s, p99 ms | 9.21 (10) | 54.2 (10) | 0.018 (10) | n/a | n/a | n/a | n/a | A |
| 240x70 at 10,000 tok/s, p50 ms | 3.67 (3) | 22.8 (2) | 0.011 (2) | 56.5 (2) | 490 (2) | 38.5 (2) | 13.4 (2) | D |
| 240x70 at 10,000 tok/s, p99 ms | 8.79 (3) | 39.3 (2) | 0.111 (2) | 108 (2) | 1,111 (2) | 250 (2) | 32.3 (2) | D |
| 240x70 at 10,000 tok/s, CPU ms | 340 (3) | 2,626 (2) | 371 (2) | 1,929 (2) | 8,840 (2) | 4,756 (2) | 9,409 (2) | D |
| 240x70 at 10,000 tok/s, peak RSS MB | 25.6 (3) | 32.0 (2) | 11.6 (2) | 350 (2) | 222 (2) | 315 (2) | 981 (2) | D |
| 50,000 tok/s, p50 ms | 3.25 (10) | 72.7 (5) | 0.023 (5) | 107 (5) | 10,798 (5) | 2,141 (5) | 1,864 (5) | E |
| 50,000 tok/s, p99 ms | 8.48 (10) | 349 (5) | 0.044 (5) | 2,432 (5) | 21,517 (5) | 7,932 (5) | 3,697 (5) | E |
| 50,000 tok/s, CPU ms | 2,983 (10) | 6,364 (5) | 3,682 (5) | 8,051 (5) | 42,117 (5) | 10,163 (5) | 25,251 (5) | E |
| 50,000 tok/s, peak RSS MB | 48.6 (10) | 110 (5) | 24.9 (5) | 526 (5) | 469 (5) | 350 (5) | 1,692 (5) | E |

## Sessions

| | nc | fx | nc-lite | Claude Code | Codex | Pi | OpenCode | Src |
|---|---:|---:|---:|---:|---:|---:|---:|---|
| 1,000 sessions stored, KB per session | 8.97 | 12.8 | 9.02 | n/a | n/a | n/a | n/a | A |
| CPU per saved session, ms | 3.16 | 1.66 | 2.66 | n/a | n/a | n/a | n/a | A |
| Listing 1,000 sessions, ms | 4.81 (20) | 6.81 (20) | 4.85 (20) | n/a | n/a | n/a | n/a | A |
| Continuing the latest of 1,000, ms | 6.79 (20) | 28.8 (20) | 8.80 (20) | n/a | n/a | n/a | n/a | A |
| Listing 14,600 sessions, ms | 588 (10) | 89.7 (10) | n/a | n/a | n/a | n/a | n/a | B |
| Listing 14,600 sessions, task-clock ms | 556 (10) | 63.7 (10) | n/a | n/a | n/a | n/a | n/a | B |
| Listing 14,600 sessions, instructions (user+kernel), M | 6,501 (10) | 598 (10) | n/a | n/a | n/a | n/a | n/a | B |
| Listing 14,600 sessions, KB printed (each CLI's default listing) | 10.5 (10) | 27.2 (10) | n/a | n/a | n/a | n/a | n/a | B |
| Continuing the latest of 14,600, ms | 596 (10) | 113 (10) | n/a | n/a | n/a | n/a | n/a | B |
| Continuing the latest of 14,600, task-clock ms | 566 (10) | 66.8 (10) | n/a | n/a | n/a | n/a | n/a | B |
| Continuing the latest of 14,600, instructions (user+kernel), M | 6,521 (10) | 607 (10) | n/a | n/a | n/a | n/a | n/a | B |

## Size

| | nc | fx | nc-lite | Claude Code | Codex | Pi | OpenCode | Src |
|---|---:|---:|---:|---:|---:|---:|---:|---|
| Install, MB | 45.4 | 12.2 | 30.1 | n/a | n/a | n/a | n/a | A |
| Main binary, MB | 45.4 | 12.2 | 30.1 | n/a | n/a | n/a | n/a | A |

## fx's own claims (unverified)

Vercel publishes these for fx. We have not reproduced them; the right-hand columns are our nearest measurement of the same thing, from the rows above.

| fx claim | fx says | fx, ours | nc, ours | Note |
|---|---:|---:|---:|---|
| Startup before terminal setup (FX_BENCH=1) | ~0.5 ms | 2.62 (A) | 2.63 (A) | our --version is a whole process lifetime |
| CI fails any command over | 2 ms | 2.62 (A) | 2.63 (A) |  |
| Cold start | 10 µs | 5.07 (A) | 5.35 (A) | ours drops the page cache first |
| Binary | 6.39 MiB | 12.2 (A) | 45.4 (A) | the linux-x86_64 release archive's binary, as installed |
| Memory | single-digit MB | 8.81 (A) | 10.3 (A) |  |
| Terminal interactions p95 | < 15 ms | 113 (A) | 0.082 (A) | ours is the slowest of 26 echoes |
| Listing 14,600 sessions | 95 s to 0.17 s | 89.7 (B) | 588 (B) |  |
| --resume last | 0.5 s | 113 (B) | 596 (B) | ours includes sending the next request |
| Shell calls | 27.9 ms | n/a | n/a | ours is per step inside one turn |
| MCP exit | 7.4 ms | n/a | n/a | not measured: no lane runs an MCP server here |

## Where nanocode does not lead yet

Every row above where nc's median is not below each of fx, nc-lite, Claude Code and Codex, with the gap to the best of them.

- Token on screen at 1,000 tok/s, p99 ms (A): nc 9.21 against nc-lite 0.018, 9.19 or 519.44x
- Token on screen at 1,000 tok/s, p50 ms (A): nc 5.03 against nc-lite 0.015, 5.01 or 331.02x
- 240x70 at 10,000 tok/s, p50 ms (D): nc 3.67 against nc-lite 0.011, 3.66 or 326.83x
- 50,000 tok/s, p99 ms (E): nc 8.48 against nc-lite 0.044, 8.44 or 193.43x
- 50,000 tok/s, p50 ms (E): nc 3.25 against nc-lite 0.023, 3.23 or 141.09x
- 240x70 at 10,000 tok/s, p99 ms (D): nc 8.79 against nc-lite 0.111, 8.68 or 79.07x
- Listing 14,600 sessions, instructions (user+kernel), M (B): nc 6,501 against fx 598, 5,903 or 10.87x
- Continuing the latest of 14,600, instructions (user+kernel), M (B): nc 6,521 against fx 607, 5,914 or 10.74x
- Listing 14,600 sessions, task-clock ms (B): nc 556 against fx 63.7, 492 or 8.73x
- Continuing the latest of 14,600, task-clock ms (B): nc 566 against fx 66.8, 499 or 8.47x
- Listing 14,600 sessions, ms (B): nc 588 against fx 89.7, 498 or 6.55x
- Continuing the latest of 14,600, ms (B): nc 596 against fx 113, 483 or 5.29x
- CPU to the first frame, ms (A): nc 3.17 against nc-lite 0.787, 2.38 or 4.03x
- Main binary, MB (A): nc 45.4 against fx 12.2, 33.2 or 3.71x; also behind nc-lite
- Install, MB (A): nc 45.4 against fx 12.2, 33.2 or 3.71x; also behind nc-lite
- Memory 1 s after the first frame, RSS MB (A): nc 10.3 against nc-lite 3.09, 7.26 or 3.35x; also behind fx
- Tool turn (Code Mode), 10 shell steps, CLI process peak RSS MB (A): nc 25.4 against fx 9.25, 16.2 or 2.75x; also behind nc-lite
- Keystroke echo p50, ms (A): nc 0.037 against nc-lite 0.014, 0.023 or 2.63x
- --version, CPU ms (A): nc 0.224 against fx 0.099, 0.125 or 2.27x
- 240x70 at 10,000 tok/s, peak RSS MB (D): nc 25.6 against nc-lite 11.6, 14.1 or 2.22x
- First frame, cold page cache, ms (A): nc 5.57 against nc-lite 2.56, 3.01 or 2.18x
- 32 runs at once, peak RSS MB each (B): nc 16.3 against fx 7.74, 8.52 or 2.10x; also behind nc-lite
- 50,000 tok/s, peak RSS MB (E): nc 48.6 against nc-lite 24.9, 23.7 or 1.95x
- CPU per saved session, ms (A): nc 3.16 against fx 1.66, 1.50 or 1.90x; also behind nc-lite
- Token on screen at 100 tok/s, p50 ms (A): nc 0.038 against nc-lite 0.021, 0.016 or 1.76x
- One-word reply, ms (A): nc 4.79 against nc-lite 2.81, 1.98 or 1.71x
- Keystroke echo, slowest of 26, ms (A): nc 0.082 against nc-lite 0.052, 0.029 or 1.56x
- CPU for that reply, ms (A): nc 2.36 against fx 1.59, 0.772 or 1.49x; also behind nc-lite
- Tool turn (Code Mode), 50 shell steps, ms (A): nc 38.0 against nc-lite 27.0, 11.0 or 1.41x
- First frame, ms (A): nc 1.19 against nc-lite 0.896, 0.291 or 1.32x
- Tool turn (Code Mode), tool result to next request p50, ms (A): nc 0.339 against nc-lite 0.302, 0.038 or 1.13x
- --version, cold page cache, ms (A): nc 5.35 against fx 5.07, 0.275 or 1.05x; also behind nc-lite
- 32 runs at once, CPU ms each (B): nc 19.2 against fx 19.1, 0.144 or 1.01x
- --version, ms (A): nc 2.63 against fx 2.62, 0.012 or 1.00x; also behind nc-lite

## nc across sources

The same nc rows from every source that has them, to show what changed between nanocode commits (and machines; see the source table).

| nc | A (d5498e784) | B (41dcbc7c4) | C (4be51211a) | D (223cb39ba) | E (de77f76e8) | F (62a0936e4) |
|---|---:|---:|---:|---:|---:|---:|
| First frame, ms | 1.19 (20) | 3.94 (20) | 1.73 (20) | 1.78 (20) |  |  |
| CPU to the first frame, ms | 3.17 (20) | 6.95 (20) | 5.48 (20) | 4.17 (20) |  |  |
| Memory 1 s after the first frame, RSS MB | 10.3 (20) | 14.7 (20) | 14.2 (20) | 14.5 (20) |  |  |
| --version, ms | 2.63 (100) | 7.11 (100) | 2.73 (100) | 2.96 (100) |  |  |
| --version, CPU ms | 0.224 (100) | 0.873 (100) | 0.494 (100) | 0.573 (100) |  |  |
| --version, cold page cache, ms | 5.35 (10) |  |  | 7.21 (5) |  |  |
| First frame, cold page cache, ms | 5.57 (10) |  |  | 9.34 (5) |  |  |
| Keystroke echo p50, ms | 0.037 (10) | 0.218 (10) |  | 0.054 (10) |  |  |
| Keystroke echo, slowest of 26, ms | 0.082 (10) | 0.320 (10) |  | 0.083 (10) |  |  |
| Idle CPU, ms per s | -0.009 (10) |  | -0.008 (10) | -0.009 (10) |  |  |
| One-word reply, ms | 4.79 (20) | 7.40 (20) | 5.12 (20) | 5.16 (20) |  |  |
| CPU for that reply, ms | 2.36 (20) | 5.63 (20) | 5.37 (20) | 4.05 (20) |  |  |
| Tool turn (Code Mode), 50 shell steps, ms | 38.0 (10) | 65.5 (10) | 33.1 (10) | 33.0 (10) |  |  |
| Tool turn (Code Mode), tool result to next request p50, ms | 0.339 (10) | 0.509 (10) | 0.276 (10) | 0.264 (10) |  |  |
| Tool turn (Code Mode), 10 shell steps, CLI process peak RSS MB | 25.4 (10) | 19.3 (10) | 18.8 (10) | 18.6 (10) |  |  |
| 32 runs at once, peak RSS MB each |  | 16.3 (10) | 15.5 (10) | 15.2 (9) |  |  |
| 32 runs at once, CPU ms each |  | 19.2 (10) | 13.5 (10) | 10.9 (9) |  |  |
| Token on screen at 100 tok/s, p50 ms | 0.038 (10) | 0.192 (10) | 0.064 (5) |  |  |  |
| Token on screen at 1,000 tok/s, p50 ms | 5.03 (10) | 4.13 (10) | 4.04 (5) |  |  |  |
| Token on screen at 1,000 tok/s, p99 ms | 9.21 (10) | 8.35 (10) | 8.16 (5) |  |  |  |
| 1,000 sessions stored, KB per session | 8.97 (1) | 5.00 (1) |  | 4.34 (1) |  |  |
| CPU per saved session, ms | 3.16 (1) | 6.79 (1) |  | 3.91 (1) |  |  |
| Listing 1,000 sessions, ms | 4.81 (20) | 13.5 (20) |  | 6.92 (20) |  |  |
| Continuing the latest of 1,000, ms | 6.79 (20) | 18.2 (20) |  | 10.9 (20) |  |  |
| Listing 14,600 sessions, ms |  | 588 (10) |  |  |  | 2,673 (10) |
| Listing 14,600 sessions, task-clock ms |  | 556 (10) |  |  |  | 932 (10) |
| Listing 14,600 sessions, instructions (user+kernel), M |  | 6,501 (10) |  |  |  | 5,247 (10) |
| Listing 14,600 sessions, KB printed (each CLI's default listing) |  | 10.5 (10) |  |  |  | 1,514 (10) |
| Continuing the latest of 14,600, ms |  | 596 (10) |  |  |  | 1,646 (10) |
| Continuing the latest of 14,600, task-clock ms |  | 566 (10) |  |  |  | 772 (10) |
| Continuing the latest of 14,600, instructions (user+kernel), M |  | 6,521 (10) |  |  |  | 4,984 (10) |
| Install, MB | 45.4 (1) | 61.9 (1) |  | 69.8 (1) |  | 44.1 (1) |
| Main binary, MB | 45.4 (1) | 45.2 (1) |  | 69.8 (1) |  | 44.1 (1) |

## nc since these runs

Numbers the source table's runs do not cover. Source A did not rebuild the 14,600-session store, so those rows above still come from B at 41dcbc7c4, before the session index (38e547873, which A's d5498e784 includes); H is that store remeasured after it, nc against fx on a shared machine, so compare within a row. Every fx number in this post is fx 0.0.16: fx 0.0.17 came out on 2026-11-14 with a smaller binary (8.4 MiB against 0.0.16's 12.2) and has not been run here. Changes that landed on nanocode master after d5498e784 are in no number in this post: TUI bytes per token (4f8526511), the streaming read buffer (e4230c13d) and nc-lite's store and binary (d83b08fa2, 89de9e518).

| Source | Machine | When (UTC) | nanocode commit |
|---|---|---|---|
| H | dev-georgios (shared), 14,600-session store, pinned to two CPUs, perf stat, 20 rounds | 2026-11-14 20:40 | 38e547873 |

| | nc | fx | nc before | Src |
|---|---:|---:|---:|---|
| Listing 14,600 sessions, CPU ms | 85 | 86 | n/a | H |
| Listing 14,600 sessions, kernel instructions, M | 37 | 98 | 166 | H |
| Listing 14,600 sessions, peak RSS MB | 32.4 | 33.2 | n/a | H |
| Continuing the latest of 14,600, CPU ms | 100 | 89 | n/a | H |
| Continuing the latest of 14,600, kernel instructions, M | 58 | 104 | 177 | H |
| Continuing the latest of 14,600, peak RSS MB | 34.3 | 34.6 | 41.0 | H |

Not measured in these sources: Tool turn, 50 shell steps, ms; Tool turn, tool result to next request p50, ms; Tool turn, 10 shell steps, CLI process peak RSS MB.

## Method

- First frame is the time from spawn until the terminal interface has drawn its prompt in a pseudo-terminal (harness/tui.py); memory is sampled from /proc/<pid>/smaps_rollup one second later, summed over the process tree. RSS counts the executable's file pages, which kernel fault-around maps in 64 KB blocks, so it depends on code layout as much as on what runs.
- Tool turn rows run one headless turn of shell steps. In the plain rows nc calls exec_command directly, its default; in the Code Mode rows it sends every step as an exec cell under --codex-orchestration code-mode, and nc rows recorded before tool_mode existed count as Code Mode. The other lanes have one way to call tools and appear in both. Their peak RSS is the largest single process's (wait4 rusage), normally the CLI itself, where other memory rows sum the process tree.
- One-word reply is a headless run from spawn to exit against a fixture that answers at once. Streaming rows replay a fixed reply at the given token rate and time each token from the fixture sending it to the pseudo-terminal showing it.
- Session rows save one-turn sessions through each CLI's own headless mode, then list them with its documented command (nc sessions, fx sessions --json) and continue the latest one (nc --continue, fx --resume last), interleaving the lanes round by round. On a loaded machine the 14,600-session rows run under perf stat, whose task-clock and instruction counts are the comparable numbers; wall times there are noise.
- Idle CPU is a 23 s session's CPU minus a 3 s session's, per second; values a few thousandths from zero are zero within measurement noise.
- Cold-cache rows drop the page cache before every sample. Install sizes leave out runtimes a lane needs but does not ship (Node for Pi).

## Reproduce

Each source directory holds COMMANDS.md (the exact commands), REPORT.md (every column, PSS and USS, errors) and the raw JSONL. A full run on one machine:

```sh
cd bench/cli
sh tools/install.sh && LANES=nclite sh tools/install.sh && . tools/install/env.sh
export NC_BIN=<nc built with scripts/build-cli.sh nanocode-cli nanocode>
ROUNDS=20 bash harness/run.sh <machine>    # every part, including the 14,600-session store, then REPORT.md and POST.md
```
