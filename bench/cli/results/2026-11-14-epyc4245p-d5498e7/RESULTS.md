# Idle-box rerun: nc d5498e784 vs fx 0.0.16 vs nc-lite d5498e784

Box: m4-metal-small FRA (AMD EPYC 4245P, 12 threads, 46 GB, Ubuntu 24.04.5, kernel 6.8.0-149, bare metal), idle (load < 1 before every step).
nc: `scripts/build-cli.sh nanocode-cli nanocode` (dist, non-PIE, BOLT 21) at d5498e7845329c2b4604b318ed2919abf6cc3340, rustc 1.100.0.
nc-lite: `PROFILE=lite scripts/build-cli.sh nc-lite nc-lite` at the same commit. fx: 0.0.16 release binary (latest.txt now says v0.0.17).
Run: `RUNSTAT_PERF=1 NETGUARD_MODE=sudo QUIET_LOAD=1 ROUNDS=20 SAMPLES=10 ONLY=nc,nclite,fx bash harness/run.sh epyc4245p-d5498e7 smoke startup coldstart firstframe coldframe idle keys oneshot tools store disk cli tui report` (harness 4a4c1190b plus RUNSTAT_PERF passed through baseEnv, so runstat-linux counts instructions). Full tables: REPORT.md. Medians.

| Row | nc | fx 0.0.16 | nc-lite |
| --- | ---: | ---: | ---: |
| First frame ms (warm / cold cache) | 1.19 / 5.57 | 1.95 / 9.35 | 0.90 / 2.56 |
| RSS / PSS / USS MB 1 s after first frame | 10.3 / 8.34 / 7.99 | 8.81 / 8.80 / 8.79 | 3.09 / 1.19 / 0.91 |
| Idle CPU ms/s | ~0 | 0.609 | ~0 |
| Key echo p50 / max ms | 0.037 / 0.082 | 0.279 / 113 | 0.014 / 0.052 |
| One-shot wall ms / CPU ms / peak RSS MB | 4.79 / 2.36 / 11.0 | 22.8 / 1.59 / 7.59 | 2.81 / 1.70 / 5.63 |
| --version warm wall ms / RSS MB / instructions | 2.63 / 2.50 / 1.30M | 2.62 / 1.38 / 0.45M | 2.62 / 2.63 / 1.60M |
| --version cold wall ms / RSS MB | 5.35 / 2.38 | 5.07 / 1.38 | 5.28 / 2.63 |
| 10-step tool turn wall ms / peak RSS MB / CPU per step ms | 14.9 / 25.6 / 1.18 | 229 / 15.2 / 2.61 | 10.9 / 12.4 / 0.80 |
| 50-step tool turn wall ms / peak RSS MB / CPU per step ms | 38.0 / 26.6 / 0.545 | 1,059 / 17.6 / 2.27 | 27.0 / 12.6 / 0.436 |
| Store 1,000 sessions KB (KB/session, files) | 8,972 (8.97, 2,007) | 12,780 (12.8, 2,005) | 9,020 (9.02, 2,003) |
| CPU per saved session ms | 3.16 | 1.66 | 2.66 |
| Listing 1,000: wall / CPU ms / RSS MB | 4.81 / 2.75 / 9.75 | 6.81 / 5.32 / 6.34 | 4.85 / 5.32 / 8.23 |
| --continue latest of 1,000: wall / CPU ms / RSS MB | 6.79 / 5.64 / 16.4 | 28.8 / 6.53 / 8.88 | 8.80 / 8.35 / 10.3 |
| Headless 10k tok/s: CPU µs/token / whole-run CPU ms / peak RSS MB | 4.33 / 217 / 15.3 | 3.47 / 174 / 11.1 | 13.3 / 665 / 8.80 |
| Headless 10k tok/s: user+kernel instructions per token (whole process, 50,000 tokens) | 36,519 | 32,732 | 222,387 |
| TUI 120x40 10k tok/s: lag p50 / p99 ms | 4.14 / 8.44 | 21.6 / 37.4 | 0.014 / 0.115 |
| TUI 120x40 10k tok/s: CPU ms / peak RSS MB / bytes per token | 464 / 21.5 / 56.6 | 2,471 / 24.5 / 31.0 | 760 / 9.74 / 6.86 |
| TUI bytes per token at 100 / 1k tok/s | 191 / 315 | 39.4 / 73.7 | 5.43 / 5.89 |
| Binary MiB | 45.4 | 12.2 | 30.1 |

Where nc still trails fx at d5498e784: terminal bytes per token (4.8x / 4.3x / 1.8x at 100 / 1k / 10k; 4f8526511, landed after this build, targets it), binary size (3.7x), headless CPU per token (+25%, +11% instructions), CPU per saved session (1.9x), peak RSS in one-shot (1.45x), tool turns (1.5-1.7x), --continue (1.84x) and --version (1.8x), --version CPU (2.3x).
nc-lite: headless streaming costs 6.8x nc's instructions per token (222k vs 36.5k) and its binary grew to 30.1 MiB from 18.2 MiB at 7837aae16.
Listing and --continue task-clock and instruction columns are n/a: BENCH_PERF was not set for the 1,000-session store step.
