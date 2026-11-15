# CLI benchmark: 2026-11-15-hosted-localfs

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
| true | 300 | 0 | 4.17 [3.36–18.7] (300) | 0.424 (300) | 1.23 (300) | 0.096 (180) | 0.039 (180) | 0.219 (180) | n/a |
| nc | 300 | 0 | 2.50 [1.19–10.3] (300) | 0.134 (300) | 0.262 (300) | 0.008 (24) | 0.008 (24) | 0.000 (24) | n/a |
| fx | 300 | 0 | 3.60 [1.39–24.5] (300) | 0.223 (300) | 1.40 (300) | 0.012 (27) | 0.012 (27) | 0.000 (27) | n/a |
| pi | 300 | 0 | 152 [129–292] (300) | 182 (300) | 104 (300) | 72.3 (300) | 51.9 (300) | 52.0 (300) | n/a |
| opencode | 300 | 0 | 290 [260–712] (300) | 377 (300) | 128 (300) | 127 (300) | 79.7 (300) | 48.6 (300) | n/a |
| claude | 300 | 0 | 10.1 [7.14–33.4] (300) | 5.63 (300) | 22.7 (300) | 16.3 (300) | 4.20 (300) | 13.7 (300) | n/a |
| codex | 300 | 0 | 5.84 [4.57–30.3] (300) | 3.18 (300) | 16.1 (300) | 9.46 (300) | 4.22 (300) | 5.24 (300) | n/a |

## Time to first frame

| lane | samples | errors | first byte ms | first frame ms | CPU ms | peak RSS MB | PSS at peak MB | anonymous at peak MB | file-backed at peak MB | processes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| nc | 9 | 0 | 2.45 (9) | 2.83 [2.26–3.54] (9) | 4.04 (9) | 5.16 (9) | 5.15 (9) | 1.24 (9) | 3.92 (9) | 0.000 (9) |
| fx | 9 | 0 | 2.62 (9) | 4.46 [3.87–14.0] (9) | 8.84 (9) | 8.85 (9) | 8.84 (9) | 1.72 (9) | 7.43 (9) | 0.000 (9) |
| pi | 9 | 0 | 231 (9) | 248 [189–349] (9) | 332 (9) | 142 (9) | 85.3 (9) | 67.8 (9) | 53.3 (9) | 1.00 (9) |
| opencode | 9 | 0 | 514 (9) | 2,634 [2,065–3,268] (9) | 5,075 (9) | 782 (9) | 775 (9) | 682 (9) | 95.7 (9) | 0.000 (9) |
| claude | 9 | 0 | 193 (9) | 311 [264–436] (9) | 647 (9) | 424 (9) | 212 (9) | 135 (9) | 79.9 (9) | 4.00 (9) |
| codex | 9 | 0 | 11.6 (9) | 379 [230–629] (9) | 321 (9) | 186 (9) | 98.9 (9) | 17.5 (9) | 85.2 (9) | 8.00 (9) |

## Memory one second after the first frame

Summed over the CLI's process tree from /proc/<pid>/smaps_rollup. RSS counts the executable's file pages, which every instance shares; PSS splits shared pages between the processes that map them; USS is memory only this tree holds. File-backed is RSS minus anonymous (binary, shared libraries, mapped files, shmem). The peak columns in other tables read smaps_rollup at the sample where the tree's summed statm RSS peaked. Runs under tools/localfs.sh, so binaries and shared libraries on a virtiofs mount count as they would from a local disk.

| lane | samples | errors | RSS MB | PSS MB | private (USS) MB | anonymous MB | file-backed MB | PSS anonymous MB | PSS file-backed MB |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| nc | 9 | 0 | 5.16 (9) | 5.15 (9) | 5.15 (9) | 1.24 (9) | 3.92 (9) | 1.24 (9) | 3.91 (9) |
| fx | 9 | 0 | 8.59 (9) | 8.58 (9) | 8.57 (9) | 1.17 (9) | 7.43 (9) | 1.17 (9) | 7.41 (9) |
| pi | 9 | 0 | 121 (9) | 89.8 (9) | 74.6 (9) | 67.4 (9) | 53.2 (9) | 67.4 (9) | 22.4 (9) |
| opencode | 9 | 0 | 759 (9) | 756 (9) | 755 (9) | 663 (9) | 95.9 (9) | 663 (9) | 93.0 (9) |
| claude | 9 | 0 | 206 (9) | 204 (9) | 203 (9) | 128 (9) | 78.1 (9) | 128 (9) | 76.2 (9) |
| codex | 9 | 0 | 157 (9) | 157 (9) | 157 (9) | 29.1 (9) | 129 (9) | 29.1 (9) | 129 (9) |

## One-shot headless prompt

| lane | samples | errors | wall ms | spawn to request ms | reply end to exit ms | CPU ms | peak RSS MB | PSS at peak MB | anonymous at peak MB | file-backed at peak MB | first request bytes | side requests | processes left behind |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| nc | 9 | 0 | 10.3 [5.88–28.5] (9) | 5.77 (9) | 2.55 (9) | 2.95 (9) | 5.37 (9) | 5.28 (9) | 0.914 (9) | 4.36 (9) | 28,943 (9) | 0.000 (9) | 0.000 (9) |
| fx | 9 | 0 | 28.6 [21.4–47.2] (9) | 16.3 (9) | 11.8 (9) | 2.78 (9) | 7.32 (9) | 7.13 (9) | 1.14 (9) | 6.00 (9) | 26,274 (9) | 0.000 (9) | 0.000 (9) |
| pi | 9 | 0 | 222 [212–351] (9) | 205 (9) | 18.7 (9) | 296 (9) | 116 (9) | 83.5 (9) | 61.9 (9) | 54.4 (9) | 5,455 (9) | 0.000 (9) | 0.000 (9) |
| opencode | 9 | 0 | 1,577 [1,534–1,962] (9) | 1,510 (9) | 69.6 (9) | 2,501 (9) | 530 (9) | 524 (9) | 450 (9) | 76.8 (9) | 30,704 (9) | 1.00 (9) | 0.000 (9) |
| claude | 9 | 0 | 294 [270–432] (9) | 257 (9) | 37.3 (9) | 304 (9) | 192 (9) | 191 (9) | 121 (9) | 71.5 (9) | 46,413 (9) | 0.000 (9) | 0.000 (9) |
| codex | 9 | 0 | 185 [154–328] (9) | 153 (9) | 26.2 (9) | 149 (9) | 157 (9) | 84.1 (9) | 15.0 (9) | 75.1 (9) | 39,563 (9) | 0.000 (9) | 0.000 (9) |

## Long tool turn: 10 shell steps, headless

| lane | samples | errors | wall ms | tool result to next request p50 ms | CPU per step ms | peak RSS MB | PSS at peak MB | anonymous at peak MB | file-backed at peak MB | last request bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| nc | 6 | 0 | 34.7 [28.6–48.7] (6) | 0.572 (6) | 1.49 (6) | 14.9 (6) | 9.35 (6) | 1.79 (6) | 13.2 (6) | 32,533 (6) |
| fx | 6 | 0 | 315 [269–392] (6) | 21.6 (6) | 6.17 (6) | 18.2 (6) | 11.4 (6) | 2.11 (6) | 16.1 (6) | 33,003 (6) |
| pi | 6 | 0 | 473 [342–690] (6) | 13.7 (6) | 42.9 (6) | 164 (6) | 96.0 (6) | 108 (6) | 56.1 (6) | 7,349 (6) |
| opencode | 6 | 0 | 2,441 [1,919–2,888] (6) | 36.7 (6) | 305 (6) | 1,105 (6) | 571 (6) | 491 (6) | 82.9 (6) | 33,163 (6) |
| claude | 6 | 0 | 775 [515–1,180] (6) | 30.1 (6) | 63.5 (6) | 455 (6) | 237 (6) | 165 (6) | 75.3 (6) | 49,685 (6) |
| codex | 6 | 0 | 1,011 [879–1,177] (6) | 68.7 (6) | 52.5 (6) | 283 (6) | 147 (6) | 31.8 (6) | 118 (6) | 43,153 (6) |
| nc (Code Mode) | 6 | 0 | 40.8 [30.0–63.7] (6) | 0.709 (6) | 1.78 (6) | 17.7 (6) | 12.4 (6) | 3.39 (6) | 14.1 (6) | 66,063 (6) |

## Where nc leads

- Long tool turn: 10 shell steps, headless, tool result to next request p50 ms: nc 0.572 against pi 13.7 (24.03x better)
- Long tool turn: 10 shell steps, headless, wall ms: nc 34.7 against fx 315 (9.10x better)
- Startup: --version, peak RSS MB: nc 0.262 against fx 1.40 (5.34x better)
- One-shot headless prompt, reply end to exit ms: nc 2.55 against fx 11.8 (4.61x better)
- Long tool turn: 10 shell steps, headless, CPU per step ms: nc 1.49 against fx 6.17 (4.13x better)
- One-shot headless prompt, spawn to request ms: nc 5.77 against fx 16.3 (2.83x better)
- One-shot headless prompt, wall ms: nc 10.3 against fx 28.6 (2.77x better)
- Time to first frame, CPU ms: nc 4.04 against fx 8.84 (2.19x better)
- Memory one second after the first frame, PSS file-backed MB: nc 3.91 against fx 7.41 (1.89x better)
- Time to first frame, file-backed at peak MB: nc 3.92 against fx 7.43 (1.89x better)
- Memory one second after the first frame, file-backed MB: nc 3.92 against fx 7.43 (1.89x better)
- Time to first frame, PSS at peak MB: nc 5.15 against fx 8.84 (1.71x better)
- Time to first frame, peak RSS MB: nc 5.16 against fx 8.85 (1.71x better)
- Startup: --version, CPU ms: nc 0.134 against fx 0.223 (1.67x better)
- Memory one second after the first frame, PSS MB: nc 5.15 against fx 8.58 (1.67x better)
- Memory one second after the first frame, RSS MB: nc 5.16 against fx 8.59 (1.67x better)
- Memory one second after the first frame, private (USS) MB: nc 5.15 against fx 8.57 (1.67x better)
- Time to first frame, first frame ms: nc 2.83 against fx 4.46 (1.57x better)
- Startup: --version, PSS at peak MB: nc 0.008 against fx 0.012 (1.50x better)
- Startup: --version, anonymous at peak MB: nc 0.008 against fx 0.012 (1.50x better)
- Startup: --version, wall ms: nc 2.50 against fx 3.60 (1.44x better)
- Time to first frame, anonymous at peak MB: nc 1.24 against fx 1.72 (1.39x better)
- One-shot headless prompt, file-backed at peak MB: nc 4.36 against fx 6.00 (1.38x better)
- One-shot headless prompt, peak RSS MB: nc 5.37 against fx 7.32 (1.36x better)
- One-shot headless prompt, PSS at peak MB: nc 5.28 against fx 7.13 (1.35x better)
- One-shot headless prompt, anonymous at peak MB: nc 0.914 against fx 1.14 (1.24x better)
- Long tool turn: 10 shell steps, headless, peak RSS MB: nc 14.9 against fx 18.2 (1.22x better)
- Long tool turn: 10 shell steps, headless, file-backed at peak MB: nc 13.2 against fx 16.1 (1.22x better)
- Long tool turn: 10 shell steps, headless, PSS at peak MB: nc 9.35 against fx 11.4 (1.22x better)
- Long tool turn: 10 shell steps, headless, anonymous at peak MB: nc 1.79 against fx 2.11 (1.18x better)
- Time to first frame, first byte ms: nc 2.45 against fx 2.62 (1.07x better)

## Where nc trails

- One-shot headless prompt, first request bytes: nc 28,943 against pi 5,455 (5.31x worse)
- Long tool turn: 10 shell steps, headless, last request bytes: nc 32,533 against pi 7,349 (4.43x worse)
- One-shot headless prompt, CPU ms: nc 2.95 against fx 2.78 (1.06x worse)
- Memory one second after the first frame, anonymous MB: nc 1.24 against fx 1.17 (1.06x worse)
- Memory one second after the first frame, PSS anonymous MB: nc 1.24 against fx 1.17 (1.06x worse)

## Ties (within 5%)

- Startup: --version, file-backed at peak MB: nc 0.000 against fx 0.000
- One-shot headless prompt, side requests: nc 0.000 against fx 0.000
- One-shot headless prompt, processes left behind: nc 0.000 against fx 0.000
