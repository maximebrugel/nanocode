# Installed CLIs

Written on Linux x86_64 (m4-metal-small FRA, AMD EPYC 4245P). The size is the main binary's; BENCH_INSTALL_<LANE> in env.sh is the whole install directory.

| Lane | Version | Install | Date (UTC) | Main binary | Runtime not counted |
| --- | --- | --- | --- | --- | --- |
| nc | nanocode 0.7.0 | LLVM_BOLT=/usr/lib/llvm-21/bin/llvm-bolt scripts/build-cli.sh nanocode-cli nanocode at https://github.com/nanocodexyz/nanocode d5498e7845329c2b4604b318ed2919abf6cc3340 (dist, non-PIE, BOLT 21, rustc 1.100.0) | 2026-11-14T22:00Z | `tools/install/nc/nanocode` (45.4 MiB) | none |
| nclite | nc-lite 0.7.0 | PROFILE=lite LLVM_BOLT=/usr/lib/llvm-21/bin/llvm-bolt scripts/build-cli.sh nc-lite nc-lite at https://github.com/nanocodexyz/nanocode d5498e7845329c2b4604b318ed2919abf6cc3340 | 2026-11-14T22:00Z | `tools/install/nclite/nc-lite` (30.1 MiB) | none |
| fx | 0.0.16 | curl https://releases.fx.sh/v0.0.16/fx-linux-x86_64.tar.gz (latest.txt is v0.0.17 since 2026-11-14; 0.0.16 pinned for comparison with earlier runs) | 2026-11-14T22:00Z | `tools/install/fx/fx` (12.2 MiB) | none |
