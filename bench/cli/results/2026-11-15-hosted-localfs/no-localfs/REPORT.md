# CLI benchmark: no-localfs

Machine: AMD EPYC 9555 64-Core Processor, 5 threads, 14 GB, Ubuntu 24.04.5 LTS, 6.18.56. 

# Installed CLIs

Written by tools/install.sh on Linux x86_64. The size is the main binary's; BENCH_INSTALL_<LANE> in env.sh is the whole install directory.

| Lane | Version | Install | Date (UTC) | Main binary | Runtime not counted |
| --- | --- | --- | --- | --- | --- |
| fx | 0.0.16 | curl https://releases.fx.sh/$(latest.txt)/fx-linux-x86_64.tar.gz (the archive https://fx.sh/setup.sh fetches; binary only) | 2026-11-15T10:36Z | `tools/install/fx/fx` (12.2 MiB) | none |
| pi | 1.5.2 | npm install --prefix tools/install/pi @earendil-works/pi-coding-agent@latest | 2026-11-15T10:25Z | `tools/install/pi/node_modules/@earendil-works/pi-coding-agent/dist/bundle/cli.js` (0.0 MiB) | Node v24.22.3 |
| opencode | 1.19.5 | npm install --prefix tools/install/opencode opencode-ai@latest (the package's bin is the native binary from its platform package) | 2026-11-15T10:25Z | `tools/install/opencode/node_modules/opencode-ai/bin/opencode.exe` (204.5 MiB) | none |
| claude | 2.1.325 (Claude Code) | npm install --prefix tools/install/claude @anthropic-ai/claude-code@latest (the package's bin is the native binary from its platform package) | 2026-11-15T10:25Z | `tools/install/claude/node_modules/@anthropic-ai/claude-code/bin/claude.exe` (348.9 MiB) | none |
| codex | codex-cli 0.169.0 | npm install --prefix tools/install/codex @openai/codex@latest (CODEX_BIN is the platform binary: the npm codex command is a Node launcher that execs it) | 2026-11-15T10:25Z | `tools/install/codex/node_modules/@openai/codex-linux-x64/vendor/x86_64-unknown-linux-musl/bin/codex` (280.6 MiB) | none (the npm launcher needs Node) |

Medians with the sample count in parentheses and [min–max] where the spread matters. CPU and memory are the whole process tree (cgroup or /proc sampling; rusage when neither is available). Lower is better everywhere.

## Startup: --version

| lane | samples | errors | wall ms | CPU ms | peak RSS MB | PSS at peak MB | anonymous at peak MB | file-backed at peak MB | instructions |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| true | 100 | 0 | 8.09 [4.74–32.7] (100) | 1.46 (100) | 1.08 (100) | 0.224 (88) | 0.043 (88) | 0.250 (88) | n/a |
| nc | 100 | 0 | 4.56 [3.18–31.0] (100) | 0.082 (100) | 0.293 (100) | n/a | n/a | n/a | n/a |
| fx | 100 | 0 | 3.87 [2.49–26.6] (100) | 0.188 (100) | 1.40 (100) | n/a | n/a | n/a | n/a |
| pi | 100 | 0 | 270 [178–364] (100) | 208 (100) | 94.3 (100) | 93.1 (100) | 51.4 (100) | 42.9 (100) | n/a |
| opencode | 100 | 0 | 411 [272–589] (100) | 387 (100) | 128 (100) | 127 (100) | 79.5 (100) | 48.2 (100) | n/a |
| claude | 100 | 0 | 14.9 [11.6–49.4] (100) | 7.24 (100) | 22.4 (100) | 19.2 (100) | 4.54 (100) | 15.2 (100) | n/a |
| codex | 100 | 0 | 9.57 [6.46–34.7] (100) | 3.18 (100) | 16.2 (100) | 9.51 (94) | 4.22 (94) | 5.27 (94) | n/a |

## Time to first frame

| lane | samples | errors | first byte ms | first frame ms | CPU ms | peak RSS MB | PSS at peak MB | anonymous at peak MB | file-backed at peak MB | processes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| nc | 3 | 0 | 4.20 (3) | 4.20 [3.27–4.61] (3) | 4.11 (3) | 5.19 (3) | 5.18 (3) | 1.23 (3) | 3.96 (3) | 0.000 (3) |
| fx | 3 | 0 | 3.96 (3) | 6.32 [6.11–6.67] (3) | 7.98 (3) | 8.72 (3) | 8.71 (3) | 1.17 (3) | 7.43 (3) | 0.000 (3) |
| pi | 3 | 0 | 260 (3) | 286 [251–310] (3) | 367 (3) | 113 (3) | 110 (3) | 69.0 (3) | 43.7 (3) | 1.00 (3) |
| opencode | 3 | 0 | 396 (3) | 2,092 [2,082–2,172] (3) | 4,759 (3) | 787 (3) | 767 (3) | 674 (3) | 94.4 (3) | 0.000 (3) |
| claude | 3 | 0 | 194 (3) | 306 [290–327] (3) | 687 (3) | 419 (3) | 210 (3) | 134 (3) | 76.2 (3) | 6.00 (3) |
| codex | 3 | 0 | 9.89 (3) | 291 [278–318] (3) | 324 (3) | 197 (3) | 160 (3) | 29.6 (3) | 130 (3) | 8.00 (3) |

## Memory one second after the first frame

Summed over the CLI's process tree from /proc/<pid>/smaps_rollup. RSS counts the executable's file pages, which every instance shares; PSS splits shared pages between the processes that map them; USS is memory only this tree holds. File-backed is RSS minus anonymous (binary, shared libraries, mapped files, shmem). The peak columns in other tables read smaps_rollup at the sample where the tree's summed statm RSS peaked. Runs under tools/localfs.sh, so binaries and shared libraries on a virtiofs mount count as they would from a local disk.

| lane | samples | errors | RSS MB | PSS MB | private (USS) MB | anonymous MB | file-backed MB | PSS anonymous MB | PSS file-backed MB |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| nc | 3 | 0 | 5.19 (3) | 5.18 (3) | 5.18 (3) | 1.23 (3) | 3.96 (3) | 1.23 (3) | 3.96 (3) |
| fx | 3 | 0 | 8.58 (3) | 8.57 (3) | 8.56 (3) | 1.16 (3) | 7.43 (3) | 1.16 (3) | 7.41 (3) |
| pi | 3 | 0 | 110 (3) | 110 (3) | 110 (3) | 67.8 (3) | 43.0 (3) | 67.8 (3) | 42.7 (3) |
| opencode | 3 | 0 | 711 (3) | 710 (3) | 709 (3) | 617 (3) | 95.2 (3) | 617 (3) | 93.9 (3) |
| claude | 3 | 0 | 200 (3) | 200 (3) | 200 (3) | 124 (3) | 76.1 (3) | 124 (3) | 75.7 (3) |
| codex | 3 | 0 | 158 (3) | 158 (3) | 158 (3) | 29.4 (3) | 128 (3) | 29.4 (3) | 128 (3) |

## One-shot headless prompt

| lane | samples | errors | wall ms | spawn to request ms | reply end to exit ms | CPU ms | peak RSS MB | PSS at peak MB | anonymous at peak MB | file-backed at peak MB | first request bytes | side requests | processes left behind |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| nc | 3 | 0 | 9.19 [7.84–15.0] (3) | 7.23 (3) | 1.60 (3) | 2.98 (3) | 5.29 (3) | 5.18 (3) | 0.832 (3) | 4.36 (3) | 28,943 (3) | 0.000 (3) | 0.000 (3) |
| fx | 3 | 0 | 28.5 [28.1–29.4] (3) | 17.6 (3) | 10.8 (3) | 2.72 (3) | 7.26 (3) | 7.14 (3) | 1.14 (3) | 6.00 (3) | 26,274 (3) | 0.000 (3) | 0.000 (3) |
| pi | 3 | 0 | 275 [262–277] (3) | 254 (3) | 17.4 (3) | 316 (3) | 109 (3) | 99.8 (3) | 62.7 (3) | 46.0 (3) | 5,455 (3) | 0.000 (3) | 0.000 (3) |
| opencode | 3 | 0 | 1,596 [1,538–1,621] (3) | 1,535 (3) | 60.3 (3) | 2,465 (3) | 530 (3) | 529 (3) | 453 (3) | 76.4 (3) | 30,704 (3) | 1.00 (3) | 0.000 (3) |
| claude | 3 | 0 | 301 [283–304] (3) | 271 (3) | 24.6 (3) | 300 (3) | 193 (3) | 192 (3) | 121 (3) | 71.6 (3) | 46,413 (3) | 0.000 (3) | 0.000 (3) |
| codex | 3 | 0 | 209 [196–229] (3) | 181 (3) | 32.1 (3) | 182 (3) | 264 (3) | 133 (3) | 24.3 (3) | 109 (3) | 39,563 (3) | 0.000 (3) | 0.000 (3) |

## Long tool turn: 10 shell steps, headless

| lane | samples | errors | wall ms | tool result to next request p50 ms | CPU per step ms | peak RSS MB | PSS at peak MB | anonymous at peak MB | file-backed at peak MB | last request bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| nc | 2 | 0 | 38.7 [35.6–41.7] (2) | 0.536 (2) | 1.94 (2) | 11.8 (2) | 9.43 (2) | 1.75 (2) | 10.0 (2) | 32,533 (2) |
| fx | 2 | 0 | 335 [299–371] (2) | 26.4 (2) | 7.71 (2) | 16.4 (2) | 11.3 (2) | 2.25 (2) | 14.1 (2) | 33,007 (2) |
| pi | 2 | 0 | 429 [418–440] (2) | 14.6 (2) | 47.3 (2) | 159 (2) | 112 (2) | 94.0 (2) | 46.7 (2) | 7,349 (2) |
| opencode | 2 | 0 | 1,847 [1,802–1,893] (2) | 30.4 (2) | 286 (2) | 1,138 (2) | 849 (2) | 729 (2) | 122 (2) | 33,163 (2) |
| claude | 2 | 0 | 629 [628–630] (2) | 27.2 (2) | 62.7 (2) | 412 (2) | 207 (2) | 135 (2) | 72.4 (2) | 49,685 (2) |
| codex | 2 | 0 | 909 [900–918] (2) | 66.3 (2) | 55.9 (2) | 160 (2) | 148 (2) | 32.1 (2) | 123 (2) | 43,153 (2) |
| nc (Code Mode) | 2 | 0 | 38.2 [33.5–43.0] (2) | 0.776 (2) | 2.15 (2) | 16.4 (2) | 12.7 (2) | 3.60 (2) | 12.9 (2) | 66,063 (2) |

## Where nc leads

- Long tool turn: 10 shell steps, headless, tool result to next request p50 ms: nc 0.536 against pi 14.6 (27.21x better)
- Long tool turn: 10 shell steps, headless, wall ms: nc 38.7 against fx 335 (8.67x better)
- One-shot headless prompt, reply end to exit ms: nc 1.60 against fx 10.8 (6.74x better)
- Startup: --version, peak RSS MB: nc 0.293 against fx 1.40 (4.77x better)
- Long tool turn: 10 shell steps, headless, CPU per step ms: nc 1.94 against fx 7.71 (3.97x better)
- One-shot headless prompt, wall ms: nc 9.19 against fx 28.5 (3.10x better)
- One-shot headless prompt, spawn to request ms: nc 7.23 against fx 17.6 (2.44x better)
- Startup: --version, CPU ms: nc 0.082 against fx 0.188 (2.29x better)
- Time to first frame, CPU ms: nc 4.11 against fx 7.98 (1.94x better)
- Memory one second after the first frame, PSS file-backed MB: nc 3.96 against fx 7.41 (1.87x better)
- Time to first frame, file-backed at peak MB: nc 3.96 against fx 7.43 (1.87x better)
- Memory one second after the first frame, file-backed MB: nc 3.96 against fx 7.43 (1.87x better)
- Time to first frame, PSS at peak MB: nc 5.18 against fx 8.71 (1.68x better)
- Time to first frame, peak RSS MB: nc 5.19 against fx 8.72 (1.68x better)
- Memory one second after the first frame, PSS MB: nc 5.18 against fx 8.57 (1.65x better)
- Memory one second after the first frame, RSS MB: nc 5.19 against fx 8.58 (1.65x better)
- Memory one second after the first frame, private (USS) MB: nc 5.18 against fx 8.56 (1.65x better)
- Time to first frame, first frame ms: nc 4.20 against fx 6.32 (1.51x better)
- Long tool turn: 10 shell steps, headless, file-backed at peak MB: nc 10.0 against fx 14.1 (1.41x better)
- Long tool turn: 10 shell steps, headless, peak RSS MB: nc 11.8 against fx 16.4 (1.38x better)
- One-shot headless prompt, file-backed at peak MB: nc 4.36 against fx 6.00 (1.38x better)
- One-shot headless prompt, PSS at peak MB: nc 5.18 against fx 7.14 (1.38x better)
- One-shot headless prompt, anonymous at peak MB: nc 0.832 against fx 1.14 (1.38x better)
- One-shot headless prompt, peak RSS MB: nc 5.29 against fx 7.26 (1.37x better)
- Long tool turn: 10 shell steps, headless, anonymous at peak MB: nc 1.75 against fx 2.25 (1.29x better)
- Long tool turn: 10 shell steps, headless, PSS at peak MB: nc 9.43 against fx 11.3 (1.20x better)

## Where nc trails

- Startup: --version, PSS at peak MB: nc failed
- Startup: --version, anonymous at peak MB: nc failed
- Startup: --version, file-backed at peak MB: nc failed
- One-shot headless prompt, first request bytes: nc 28,943 against pi 5,455 (5.31x worse)
- Long tool turn: 10 shell steps, headless, last request bytes: nc 32,533 against pi 7,349 (4.43x worse)
- Startup: --version, wall ms: nc 4.56 against fx 3.87 (1.18x worse)
- One-shot headless prompt, CPU ms: nc 2.98 against fx 2.72 (1.09x worse)
- Memory one second after the first frame, anonymous MB: nc 1.23 against fx 1.16 (1.06x worse)
- Memory one second after the first frame, PSS anonymous MB: nc 1.23 against fx 1.16 (1.06x worse)
- Time to first frame, first byte ms: nc 4.20 against fx 3.96 (1.06x worse)
- Time to first frame, anonymous at peak MB: nc 1.23 against fx 1.17 (1.06x worse)

## Ties (within 5%)

- One-shot headless prompt, side requests: nc 0.000 against fx 0.000
- One-shot headless prompt, processes left behind: nc 0.000 against fx 0.000
