# CLI benchmark: 2026-11-14-epyc4245p-d5498e7

Machine: AMD EPYC 4245P 6-Core Processor, 12 threads, 46 GB, Ubuntu 24.04.5 LTS, 6.8.0-149-generic. 

# Installed CLIs

Written on Linux x86_64 (m4-metal-small FRA, AMD EPYC 4245P). The size is the main binary's; BENCH_INSTALL_<LANE> in env.sh is the whole install directory.

| Lane | Version | Install | Date (UTC) | Main binary | Runtime not counted |
| --- | --- | --- | --- | --- | --- |
| nc | nanocode 0.7.0 | LLVM_BOLT=/usr/lib/llvm-21/bin/llvm-bolt scripts/build-cli.sh nanocode-cli nanocode at https://github.com/nanocodexyz/nanocode d5498e7845329c2b4604b318ed2919abf6cc3340 (dist, non-PIE, BOLT 21, rustc 1.100.0) | 2026-11-14T22:00Z | `tools/install/nc/nanocode` (45.4 MiB) | none |
| nclite | nc-lite 0.7.0 | PROFILE=lite LLVM_BOLT=/usr/lib/llvm-21/bin/llvm-bolt scripts/build-cli.sh nc-lite nc-lite at https://github.com/nanocodexyz/nanocode d5498e7845329c2b4604b318ed2919abf6cc3340 | 2026-11-14T22:00Z | `tools/install/nclite/nc-lite` (30.1 MiB) | none |
| fx | 0.0.16 | curl https://releases.fx.sh/v0.0.16/fx-linux-x86_64.tar.gz (latest.txt is v0.0.17 since 2026-11-14; 0.0.16 pinned for comparison with earlier runs) | 2026-11-14T22:00Z | `tools/install/fx/fx` (12.2 MiB) | none |

Medians with the sample count in parentheses and [min–max] where the spread matters. CPU and memory are the whole process tree (cgroup or /proc sampling; rusage when neither is available). Lower is better everywhere.

## Startup: --version

| lane | samples | errors | wall ms | CPU ms | peak RSS MB | instructions |
|---|---:|---:|---:|---:|---:|---:|
| true | 100 | 0 | 2.67 [2.59–114] (100) | 0.151 (100) | 1.25 (100) | 737,432 (100) |
| nc | 100 | 0 | 2.63 [2.58–2.82] (100) | 0.224 (100) | 2.50 (100) | 1,302,363 (100) |
| fx | 100 | 0 | 2.62 [1.15–2.97] (100) | 0.099 (100) | 1.38 (100) | 449,249 (100) |
| nclite | 100 | 0 | 2.62 [2.58–2.80] (100) | 0.255 (100) | 2.63 (100) | 1,602,427 (100) |

## Cold start: --version after dropping the page cache

| lane | samples | errors | wall ms | CPU ms | peak RSS MB |
|---|---:|---:|---:|---:|---:|
| nc | 10 | 0 | 5.35 [5.27–7.37] (10) | 0.671 (10) | 2.38 (10) |
| fx | 10 | 0 | 5.07 [4.98–14.4] (10) | 0.619 (10) | 1.38 (10) |
| nclite | 10 | 0 | 5.28 [5.22–5.47] (10) | 0.579 (10) | 2.63 (10) |

## Time to first frame

| lane | samples | errors | first byte ms | first frame ms | CPU ms | peak RSS MB | processes |
|---|---:|---:|---:|---:|---:|---:|---:|
| nc | 20 | 0 | 1.04 (20) | 1.19 [1.06–5.03] (20) | 3.17 (20) | 10.4 (20) | 0.000 (20) |
| fx | 20 | 0 | 0.986 (20) | 1.95 [1.75–6.50] (20) | 4.26 (20) | 8.98 (20) | 0.000 (20) |
| nclite | 20 | 0 | 0.896 (20) | 0.896 [0.812–1.70] (20) | 0.787 (20) | 3.09 (20) | 0.000 (20) |

## Memory one second after the first frame

Summed over the CLI's process tree from /proc/<pid>/smaps_rollup. RSS counts the executable's file pages, which every instance shares; PSS splits shared pages between the processes that map them; USS is memory only this tree holds.

| lane | samples | errors | RSS MB | PSS MB | private (USS) MB | anonymous MB |
|---|---:|---:|---:|---:|---:|---:|
| nc | 20 | 0 | 10.3 (20) | 8.34 (20) | 7.99 (20) | 2.06 (20) |
| fx | 20 | 0 | 8.81 (20) | 8.80 (20) | 8.79 (20) | 1.25 (20) |
| nclite | 20 | 0 | 3.09 (20) | 1.19 (20) | 0.906 (20) | 0.254 (20) |

## Time to first frame, cold page cache

| lane | samples | errors | first frame ms | CPU ms |
|---|---:|---:|---:|---:|
| nc | 10 | 0 | 5.57 [5.27–5.97] (10) | 5.09 (10) |
| fx | 10 | 0 | 9.35 [9.09–9.65] (10) | 5.95 (10) |
| nclite | 10 | 0 | 2.56 [2.27–2.68] (10) | 1.10 (10) |

## Idle terminal interface

| lane | samples | errors | idle CPU ms/s | bytes written while idle | peak RSS MB | processes |
|---|---:|---:|---:|---:|---:|---:|
| nc | 10 | 0 | -0.009 [-0.083–0.012] (10) | 0.000 (10) | 10.3 (10) | 0.000 (10) |
| fx | 10 | 0 | 0.609 [0.442–0.730] (10) | 0.000 (10) | 9.37 (10) | 0.000 (10) |
| nclite | 10 | 0 | -0.003 [-0.017–0.008] (10) | 0.000 (10) | 3.09 (10) | 0.000 (10) |

## Keystroke echo

| lane | samples | errors | echo p50 ms | echo max ms | keys echoed of 26 |
|---|---:|---:|---:|---:|---:|
| nc | 10 | 0 | 0.037 [0.031–0.039] (10) | 0.082 (10) | 26.0 (10) |
| fx | 10 | 0 | 0.279 [0.169–0.329] (10) | 113 (10) | 26.0 (10) |
| nclite | 10 | 0 | 0.014 [0.013–0.015] (10) | 0.052 (10) | 26.0 (10) |

## One-shot headless prompt

| lane | samples | errors | wall ms | spawn to request ms | reply end to exit ms | CPU ms | peak RSS MB | first request bytes | side requests | processes left behind |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| nc | 20 | 0 | 4.79 [2.77–7.82] (20) | 2.14 (20) | 2.35 (20) | 2.36 (20) | 11.0 (20) | 62,251 (20) | 0.000 (20) | 0.000 (20) |
| fx | 20 | 0 | 22.8 [22.7–24.8] (20) | 12.0 (20) | 10.7 (20) | 1.59 (20) | 7.59 (20) | 26,280 (20) | 0.000 (20) | 0.000 (20) |
| nclite | 20 | 0 | 2.81 [2.75–4.97] (20) | 1.68 (20) | 0.998 (20) | 1.70 (20) | 5.63 (20) | 23,991 (20) | 0.000 (20) | 0.000 (20) |

## Long tool turn: 10 shell steps, headless

| lane | samples | errors | wall ms | tool result to next request p50 ms | CPU per step ms | peak RSS MB | last request bytes |
|---|---:|---:|---:|---:|---:|---:|---:|
| nc | 10 | 0 | 14.9 [14.8–31.7] (10) | 0.377 (10) | 1.18 (10) | 25.6 (10) | 66,555 (10) |
| fx | 10 | 0 | 229 [229–231] (10) | 20.4 (10) | 2.61 (10) | 15.2 (10) | 33,006 (10) |
| nclite | 10 | 0 | 10.9 [10.8–17.4] (10) | 0.314 (10) | 0.801 (10) | 12.4 (10) | 27,581 (10) |

## Long tool turn: 50 shell steps, headless

| lane | samples | errors | wall ms | tool result to next request p50 ms | CPU per step ms | peak RSS MB | last request bytes |
|---|---:|---:|---:|---:|---:|---:|---:|
| nc | 10 | 0 | 38.0 [35.0–39.0] (10) | 0.339 (10) | 0.545 (10) | 26.6 (10) | 83,675 (10) |
| fx | 10 | 0 | 1,059 [1,057–1,063] (10) | 20.5 (10) | 2.27 (10) | 17.6 (10) | 59,766 (10) |
| nclite | 10 | 0 | 27.0 [23.1–29.1] (10) | 0.302 (10) | 0.436 (10) | 12.6 (10) | 41,821 (10) |

## Session store: 1,000 one-turn sessions

| lane | samples | errors | store KB | KB per session | files | CPU per saved session ms |
|---|---:|---:|---:|---:|---:|---:|
| nc | 1 | 0 | 8,972 (1) | 8.97 (1) | 2,007 (1) | 3.16 (1) |
| fx | 1 | 0 | 12,780 (1) | 12.8 (1) | 2,005 (1) | 1.66 (1) |
| nclite | 1 | 0 | 9,020 (1) | 9.02 (1) | 2,003 (1) | 2.66 (1) |

## Listing 1,000 sessions

| lane | samples | errors | wall ms | CPU ms | peak RSS MB | task-clock ms | user instructions | kernel instructions |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| nc | 20 | 0 | 4.81 [2.81–4.86] (20) | 2.75 (20) | 9.75 (20) | n/a | n/a | n/a |
| fx | 20 | 0 | 6.81 [6.77–6.91] (20) | 5.32 (20) | 6.34 (20) | n/a | n/a | n/a |
| nclite | 20 | 0 | 4.85 [4.74–6.86] (20) | 5.32 (20) | 8.23 (20) | n/a | n/a | n/a |

## Continuing the latest of 1,000 sessions

| lane | samples | errors | wall ms | spawn to request ms | CPU ms | peak RSS MB | task-clock ms | user instructions | kernel instructions |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| nc | 20 | 0 | 6.79 [6.74–8.81] (20) | 4.49 (20) | 5.64 (20) | 16.4 (20) | n/a | n/a | n/a |
| fx | 20 | 0 | 28.8 [28.7–28.8] (20) | 16.5 (20) | 6.53 (20) | 8.88 (20) | n/a | n/a | n/a |
| nclite | 20 | 0 | 8.80 [8.76–12.8] (20) | 6.32 (20) | 8.35 (20) | 10.3 (20) | n/a | n/a | n/a |

## Install size

Runtimes a lane needs but does not install (Node for npm packages) are not counted; VERSIONS.md lists them.

| lane | samples | errors | install MB | main binary MB |
|---|---:|---:|---:|---:|
| nc | 1 | 0 | 45.4 (1) | 45.4 (1) |
| fx | 1 | 0 | 12.2 (1) | 12.2 (1) |
| nclite | 1 | 0 | 30.1 (1) | 30.1 (1) |

## Streaming, headless

**per-token lag p50 ms**

| lane | 100 tok/s | 1,000 tok/s | 10,000 tok/s |
|---|---:|---:|---:|
| nc | 0.014 (10) | 0.008 (10) | 0.008 (10) |
| fx | 0.010 (10) | 0.007 (10) | 0.007 (10) |
| nclite | 0.018 (10) | 0.014 (10) | 0.013 (10) |

**per-token lag p99 ms**

| lane | 100 tok/s | 1,000 tok/s | 10,000 tok/s |
|---|---:|---:|---:|
| nc | 0.022 (10) | 0.010 (10) | 0.009 (10) |
| fx | 0.020 (10) | 0.009 (10) | 0.008 (10) |
| nclite | 0.032 (10) | 0.016 (10) | 0.015 (10) |

**stream end to exit ms**

| lane | 100 tok/s | 1,000 tok/s | 10,000 tok/s |
|---|---:|---:|---:|
| nc | 2.03 (10) | 2.72 (10) | 4.72 (10) |
| fx | 6.39 (10) | 7.10 (10) | 7.68 (10) |
| nclite | 0.858 (10) | 1.50 (10) | 5.52 (10) |

**CPU, whole run ms**

| lane | 100 tok/s | 1,000 tok/s | 10,000 tok/s |
|---|---:|---:|---:|
| nc | 6.79 (10) | 25.2 (10) | 217 (10) |
| fx | 5.48 (10) | 20.5 (10) | 174 (10) |
| nclite | 10.8 (10) | 68.1 (10) | 665 (10) |

**CPU per token µs**

| lane | 100 tok/s | 1,000 tok/s | 10,000 tok/s |
|---|---:|---:|---:|
| nc | 13.6 (10) | 5.04 (10) | 4.33 (10) |
| fx | 11.0 (10) | 4.10 (10) | 3.47 (10) |
| nclite | 21.6 (10) | 13.6 (10) | 13.3 (10) |

**peak RSS MB**

| lane | 100 tok/s | 1,000 tok/s | 10,000 tok/s |
|---|---:|---:|---:|
| nc | 11.2 (10) | 11.7 (10) | 15.3 (10) |
| fx | 7.57 (10) | 7.88 (10) | 11.1 (10) |
| nclite | 5.88 (10) | 6.13 (10) | 8.80 (10) |

**tokens never on stdout**

| lane | 100 tok/s | 1,000 tok/s | 10,000 tok/s |
|---|---:|---:|---:|
| nc | 0.000 (10) | 0.000 (10) | 0.000 (10) |
| fx | 0.000 (10) | 0.000 (10) | 0.000 (10) |
| nclite | 0.000 (10) | 0.000 (10) | 0.000 (10) |

## Streaming, terminal interface 120x40

**on-screen lag p50 ms**

| lane | 100 tok/s | 1,000 tok/s | 10,000 tok/s |
|---|---:|---:|---:|
| nc | 0.038 (10) | 5.03 (10) | 4.14 (10) |
| fx | 200 (10) | 32.6 (10) | 21.6 (10) |
| nclite | 0.021 (10) | 0.015 (10) | 0.014 (10) |

**on-screen lag p99 ms**

| lane | 100 tok/s | 1,000 tok/s | 10,000 tok/s |
|---|---:|---:|---:|
| nc | 0.160 (10) | 9.21 (10) | 8.44 (10) |
| fx | 403 (10) | 54.2 (10) | 37.4 (10) |
| nclite | 0.033 (10) | 0.018 (10) | 0.115 (10) |

**stream end to last token on screen ms**

| lane | 100 tok/s | 1,000 tok/s | 10,000 tok/s |
|---|---:|---:|---:|
| nc | 0.023 (10) | 0.104 (10) | 1.23 (10) |
| fx | 17.3 (10) | 13.8 (10) | 31.5 (10) |
| nclite | 0.013 (10) | 0.006 (10) | -0.123 (10) |

**Enter to request ms**

| lane | 100 tok/s | 1,000 tok/s | 10,000 tok/s |
|---|---:|---:|---:|
| nc | 0.736 (10) | 0.730 (10) | 0.773 (10) |
| fx | 3.22 (10) | 3.06 (10) | 3.16 (10) |
| nclite | 1.34 (10) | 1.19 (10) | 1.43 (10) |

**CPU, whole session ms**

| lane | 100 tok/s | 1,000 tok/s | 10,000 tok/s |
|---|---:|---:|---:|
| nc | 28.9 (10) | 71.0 (10) | 464 (10) |
| fx | 49.9 (10) | 238 (10) | 2,471 (10) |
| nclite | 13.7 (10) | 79.4 (10) | 760 (10) |

**peak RSS MB**

| lane | 100 tok/s | 1,000 tok/s | 10,000 tok/s |
|---|---:|---:|---:|
| nc | 14.7 (10) | 15.4 (10) | 21.5 (10) |
| fx | 12.4 (10) | 16.1 (10) | 24.5 (10) |
| nclite | 6.58 (10) | 7.01 (10) | 9.74 (10) |

**terminal bytes per token**

| lane | 100 tok/s | 1,000 tok/s | 10,000 tok/s |
|---|---:|---:|---:|
| nc | 191 (10) | 315 (10) | 56.6 (10) |
| fx | 39.4 (10) | 73.7 (10) | 31.0 (10) |
| nclite | 5.43 (10) | 5.89 (10) | 6.86 (10) |

**tokens never on screen**

| lane | 100 tok/s | 1,000 tok/s | 10,000 tok/s |
|---|---:|---:|---:|
| nc | 0.000 (10) | 0.000 (10) | 0.000 (10) |
| fx | 0.000 (10) | 0.000 (10) | 0.000 (10) |
| nclite | 0.000 (10) | 0.000 (10) | 0.000 (10) |

## Where nc leads

- Idle terminal interface, idle CPU ms/s: nc -0.009 against fx 0.609 (-66.21x better)
- Streaming TUI 120x40 at 100 tok/s, on-screen lag p50 ms: nc 0.038 against fx 200 (5284.06x better)
- Streaming TUI 120x40 at 100 tok/s, on-screen lag p99 ms: nc 0.160 against fx 403 (2523.14x better)
- Keystroke echo, echo max ms: nc 0.082 against fx 113 (1387.88x better)
- Streaming TUI 120x40 at 100 tok/s, stream end to last token on screen ms: nc 0.023 against fx 17.3 (759.82x better)
- Streaming TUI 120x40 at 1,000 tok/s, stream end to last token on screen ms: nc 0.104 against fx 13.8 (132.44x better)
- Long tool turn: 50 shell steps, headless, tool result to next request p50 ms: nc 0.339 against fx 20.5 (60.40x better)
- Long tool turn: 10 shell steps, headless, tool result to next request p50 ms: nc 0.377 against fx 20.4 (54.08x better)
- Long tool turn: 50 shell steps, headless, wall ms: nc 38.0 against fx 1,059 (27.88x better)
- Streaming TUI 120x40 at 10,000 tok/s, stream end to last token on screen ms: nc 1.23 against fx 31.5 (25.53x better)
- Long tool turn: 10 shell steps, headless, wall ms: nc 14.9 against fx 229 (15.38x better)
- Keystroke echo, echo p50 ms: nc 0.037 against fx 0.279 (7.57x better)
- Streaming TUI 120x40 at 1,000 tok/s, on-screen lag p50 ms: nc 5.03 against fx 32.6 (6.49x better)
- Streaming TUI 120x40 at 1,000 tok/s, on-screen lag p99 ms: nc 9.21 against fx 54.2 (5.89x better)
- One-shot headless prompt, spawn to request ms: nc 2.14 against fx 12.0 (5.60x better)
- Streaming TUI 120x40 at 10,000 tok/s, CPU, whole session ms: nc 464 against fx 2,471 (5.32x better)
- Streaming TUI 120x40 at 10,000 tok/s, on-screen lag p50 ms: nc 4.14 against fx 21.6 (5.22x better)
- One-shot headless prompt, wall ms: nc 4.79 against fx 22.8 (4.77x better)
- One-shot headless prompt, reply end to exit ms: nc 2.35 against fx 10.7 (4.55x better)
- Streaming TUI 120x40 at 10,000 tok/s, on-screen lag p99 ms: nc 8.44 against fx 37.4 (4.43x better)
- Streaming TUI 120x40 at 100 tok/s, Enter to request ms: nc 0.736 against fx 3.22 (4.38x better)
- Continuing the latest of 1,000 sessions, wall ms: nc 6.79 against fx 28.8 (4.24x better)
- Streaming TUI 120x40 at 1,000 tok/s, Enter to request ms: nc 0.730 against fx 3.06 (4.20x better)
- Long tool turn: 50 shell steps, headless, CPU per step ms: nc 0.545 against fx 2.27 (4.16x better)
- Streaming TUI 120x40 at 10,000 tok/s, Enter to request ms: nc 0.773 against fx 3.16 (4.08x better)
- Continuing the latest of 1,000 sessions, spawn to request ms: nc 4.49 against fx 16.5 (3.68x better)
- Streaming TUI 120x40 at 1,000 tok/s, CPU, whole session ms: nc 71.0 against fx 238 (3.35x better)
- Streaming headless at 100 tok/s, stream end to exit ms: nc 2.03 against fx 6.39 (3.14x better)
- Streaming headless at 1,000 tok/s, stream end to exit ms: nc 2.72 against fx 7.10 (2.61x better)
- Long tool turn: 10 shell steps, headless, CPU per step ms: nc 1.18 against fx 2.61 (2.20x better)
- Listing 1,000 sessions, CPU ms: nc 2.75 against fx 5.32 (1.93x better)
- Streaming TUI 120x40 at 100 tok/s, CPU, whole session ms: nc 28.9 against fx 49.9 (1.73x better)
- Time to first frame, cold page cache, first frame ms: nc 5.57 against fx 9.35 (1.68x better)
- Time to first frame, first frame ms: nc 1.19 against fx 1.95 (1.65x better)
- Streaming headless at 10,000 tok/s, stream end to exit ms: nc 4.72 against fx 7.68 (1.63x better)
- Session store: 1,000 one-turn sessions, store KB: nc 8,972 against fx 12,780 (1.42x better)
- Session store: 1,000 one-turn sessions, KB per session: nc 8.97 against fx 12.8 (1.42x better)
- Listing 1,000 sessions, wall ms: nc 4.81 against fx 6.81 (1.42x better)
- Time to first frame, CPU ms: nc 3.17 against fx 4.26 (1.34x better)
- Time to first frame, cold page cache, CPU ms: nc 5.09 against fx 5.95 (1.17x better)
- Continuing the latest of 1,000 sessions, CPU ms: nc 5.64 against fx 6.53 (1.16x better)
- Streaming TUI 120x40 at 10,000 tok/s, peak RSS MB: nc 21.5 against fx 24.5 (1.14x better)
- Memory one second after the first frame, private (USS) MB: nc 7.99 against fx 8.79 (1.10x better)
- Memory one second after the first frame, PSS MB: nc 8.34 against fx 8.80 (1.06x better)

## Where nc trails

- Listing 1,000 sessions, task-clock ms: nc failed
- Listing 1,000 sessions, user instructions: nc failed
- Listing 1,000 sessions, kernel instructions: nc failed
- Continuing the latest of 1,000 sessions, task-clock ms: nc failed
- Continuing the latest of 1,000 sessions, user instructions: nc failed
- Continuing the latest of 1,000 sessions, kernel instructions: nc failed
- Streaming TUI 120x40 at 100 tok/s, terminal bytes per token: nc 191 against fx 39.4 (4.84x worse)
- Streaming TUI 120x40 at 1,000 tok/s, terminal bytes per token: nc 315 against fx 73.7 (4.27x worse)
- Install size, main binary MB: nc 45.4 against fx 12.2 (3.71x worse)
- Install size, install MB: nc 45.4 against fx 12.2 (3.71x worse)
- One-shot headless prompt, first request bytes: nc 62,251 against fx 26,280 (2.37x worse)
- Startup: --version, CPU ms: nc 0.224 against fx 0.099 (2.27x worse)
- Long tool turn: 10 shell steps, headless, last request bytes: nc 66,555 against fx 33,006 (2.02x worse)
- Session store: 1,000 one-turn sessions, CPU per saved session ms: nc 3.16 against fx 1.66 (1.90x worse)
- Continuing the latest of 1,000 sessions, peak RSS MB: nc 16.4 against fx 8.88 (1.84x worse)
- Streaming TUI 120x40 at 10,000 tok/s, terminal bytes per token: nc 56.6 against fx 31.0 (1.82x worse)
- Startup: --version, peak RSS MB: nc 2.50 against fx 1.38 (1.82x worse)
- Cold start: --version after dropping the page cache, peak RSS MB: nc 2.38 against fx 1.38 (1.73x worse)
- Long tool turn: 10 shell steps, headless, peak RSS MB: nc 25.6 against fx 15.2 (1.69x worse)
- Memory one second after the first frame, anonymous MB: nc 2.06 against fx 1.25 (1.65x worse)
- Listing 1,000 sessions, peak RSS MB: nc 9.75 against fx 6.34 (1.54x worse)
- Long tool turn: 50 shell steps, headless, peak RSS MB: nc 26.6 against fx 17.6 (1.51x worse)
- Streaming headless at 1,000 tok/s, peak RSS MB: nc 11.7 against fx 7.88 (1.49x worse)
- One-shot headless prompt, CPU ms: nc 2.36 against fx 1.59 (1.49x worse)
- Streaming headless at 100 tok/s, peak RSS MB: nc 11.2 against fx 7.57 (1.48x worse)
- One-shot headless prompt, peak RSS MB: nc 11.0 against fx 7.59 (1.45x worse)
- Long tool turn: 50 shell steps, headless, last request bytes: nc 83,675 against fx 59,766 (1.40x worse)
- Streaming headless at 10,000 tok/s, peak RSS MB: nc 15.3 against fx 11.1 (1.38x worse)
- Streaming headless at 10,000 tok/s, CPU, whole run ms: nc 217 against fx 174 (1.25x worse)
- Streaming headless at 10,000 tok/s, CPU per token µs: nc 4.33 against fx 3.47 (1.25x worse)
- Streaming headless at 100 tok/s, CPU, whole run ms: nc 6.79 against fx 5.48 (1.24x worse)
- Streaming headless at 100 tok/s, CPU per token µs: nc 13.6 against fx 11.0 (1.24x worse)
- Streaming headless at 1,000 tok/s, CPU, whole run ms: nc 25.2 against fx 20.5 (1.23x worse)
- Streaming headless at 1,000 tok/s, CPU per token µs: nc 5.04 against fx 4.10 (1.23x worse)
- Streaming TUI 120x40 at 100 tok/s, peak RSS MB: nc 14.7 against fx 12.4 (1.18x worse)
- Memory one second after the first frame, RSS MB: nc 10.3 against fx 8.81 (1.17x worse)
- Time to first frame, peak RSS MB: nc 10.4 against fx 8.98 (1.16x worse)
- Idle terminal interface, peak RSS MB: nc 10.3 against fx 9.37 (1.10x worse)
- Cold start: --version after dropping the page cache, CPU ms: nc 0.671 against fx 0.619 (1.08x worse)
- Cold start: --version after dropping the page cache, wall ms: nc 5.35 against fx 5.07 (1.05x worse)

## Ties (within 5%)

- Startup: --version, wall ms: nc 2.63 against fx 2.62
- Time to first frame, first byte ms: nc 1.04 against fx 0.986
- Idle terminal interface, bytes written while idle: nc 0.000 against fx 0.000
- One-shot headless prompt, side requests: nc 0.000 against fx 0.000
- One-shot headless prompt, processes left behind: nc 0.000 against fx 0.000
- Session store: 1,000 one-turn sessions, files: nc 2,007 against fx 2,005
- Streaming headless at 100 tok/s, per-token lag p50 ms: nc 0.014 against fx 0.010
- Streaming headless at 1,000 tok/s, per-token lag p50 ms: nc 0.008 against fx 0.007
- Streaming headless at 10,000 tok/s, per-token lag p50 ms: nc 0.008 against fx 0.007
- Streaming headless at 100 tok/s, per-token lag p99 ms: nc 0.022 against fx 0.020
- Streaming headless at 1,000 tok/s, per-token lag p99 ms: nc 0.010 against fx 0.009
- Streaming headless at 10,000 tok/s, per-token lag p99 ms: nc 0.009 against fx 0.008
- Streaming headless at 100 tok/s, tokens never on stdout: nc 0.000 against fx 0.000
- Streaming headless at 1,000 tok/s, tokens never on stdout: nc 0.000 against fx 0.000
- Streaming headless at 10,000 tok/s, tokens never on stdout: nc 0.000 against fx 0.000
- Streaming TUI 120x40 at 1,000 tok/s, peak RSS MB: nc 15.4 against fx 16.1
- Streaming TUI 120x40 at 100 tok/s, tokens never on screen: nc 0.000 against fx 0.000
- Streaming TUI 120x40 at 1,000 tok/s, tokens never on screen: nc 0.000 against fx 0.000
- Streaming TUI 120x40 at 10,000 tok/s, tokens never on screen: nc 0.000 against fx 0.000
