# Notes

- nc: nanocode at 2a533981c, built with `scripts/build-cli.sh nanocode-cli nanocode` (dist profile, BOLT, static glibc agent and static front, x86_64-unknown-linux-gnu). NC_BIN is the front; nanocode-agent is beside it. Both binaries are static, so nc maps no shared library.
- fx is pinned to 0.0.16 (`FX_VERSION=v0.0.16 LANES=fx sh tools/install.sh`): fx 0.0.17 prints "Offline · retrying" forever against the fixture, with or without tools/netguard.sh.
- Machine: hosted Linux VM, 5 cores of an EPYC 9555, 14 GB, load 2 to 5 from other work during the run. Times are noisy. Memory is the purpose of this run.
- Three passes of startup (100 rounds), oneshot (3), firstframe (3) and tools (10 steps, 2 rounds), lanes interleaved inside each pass, then one pass with LOCALFS=0 in `no-localfs/`.
- `/` on this machine is virtiofs. Every pass ran under tools/localfs.sh, which bind-mounted 22 files from ext4 copies: node, bash, dash, ls, ld-linux-x86-64.so.2, libc.so.6, libm, libstdc++, libgcc_s, libnss_* and their dependencies. The lane binaries other than node are installed under tools/install on ext4.
- Without localfs (`no-localfs/REPORT.md`), file pages of the virtiofs files count in RSS only some of the time: libc.so.6 in a fresh `sleep` read 24 kB in two runs and 1,344 kB in a third. With localfs, pi's file-backed peak at --version is 52.0 MB against 42.9 MB, the node binary alone 41.3 MB against 31.5 MB.
- PSS of pi splits the node binary's pages with the harness's own node processes (suite.mjs and fakellm.mjs map the same file in the same namespace). Compare pi by RSS.
- Peak PSS, anonymous and file-backed columns come from smaps_rollup at the sampler pass where the tree's statm RSS peaked. A run that exits before the first 2 ms sample (nc and fx --version) has no peak breakdown, and its peak RSS is rusage maxrss.
