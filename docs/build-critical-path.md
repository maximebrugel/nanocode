# Build critical path: ranked structural changes

Measured 2026-11-20 at master 0189c135a on a 48-core Linux box (shared, load about 16), with
\`nc-build analyze\` (cargo metadata, 30 days of git log, and \`cargo --timings\`) plus edit benchmarks:
five interleaved runs per edit, each adding one new \`pub fn\` to the edited crate, then
\`cargo check --workspace --all-targets\`; medians below. Compiler wrappers stayed on; the cold runs used a
fresh \`XDG_CACHE_HOME\` and \`NANOCODE_BUILDCACHE_URL=off\` so nothing came from a cache.

## Baseline

| build | wall |
|---|---|
| nanocode cold check, empty caches | 156.4 s (cpu 461.6 s, ideal critical path 129.5 s) |
| nanocode cold check, new target dir, warm local caches | 9 s |
| nanocode edit nanocode-sys (83 dependents) | 35.1 s (94.1 / 35.1 / 35.4 / 34.3 / 33.5) |
| nanocode edit nanocode-code-mode (13 dependents) | 3.8 s |
| nanocode edit nanocode-backend-claude (3 dependents) | 2.7 s |
| nanocode edit nanocode-sim (leaf) | 2.0 s |
| pyramid2 workspace edit core / engine | 1.2 s / 0.9 s |
| pyramid2 server (own workspace) check after core / engine / server edit | 2.45 / 2.12 / 1.69 s |
| pyramid2 server leaf edit: build / test --no-run | 3.27 / 3.82 s |
| pyramid2 engine edit, test --no-run, opt-level 2 vs 0 | 2.58 vs 1.99 s |

Cold critical path: python-capi build script compile 1.0 s > run 112.8 s > python-capi 2.4 s > python 2.2 s > nanocode-sim bin 11.2 s.

nanocode-sys edit critical path (42.4 s with --timings): sys 0.4 s > compression 0.6 s > rustpython-unicode build script 1.4 + 4.5 s > ruff/rustpython chain > rustpython-vm 9.7 s; **nanocode-python-data build script run 30.0 s** sits beside it and is the largest unit.

No duplicated packages, no library compiled twice for different features, 3 proc-macros (rustpython-derive 3.0 s cold is the only one that matters), 12 build scripts, 2 test binaries: dependency hygiene, dev-dependencies and test harnesses are not where the time is.

## Ranked changes

1. **Stop python-data's build script rerunning on core edits (about 18-22 s per affected edit).**
   It build-depends on nanocode-compression (which pulls nanocode-sys and djs-libm) and rustpython-compiler, so any
   edit in that closure relinks the build script and reruns it: 30.0 s of the 42.4 s sys-edit build. 175 of 2558
   commits in 30 days touched that closure (79 touched compression, sys or djs-libm alone). Fix: key the compiled
   frozen bytecode by a hash of the Python sources, build.rs and the rustpython compiler sources, cached like
   python-capi's archive (bd9cd5994), and redo only the deflate step on a hit; or compress with something outside
   the workspace's core crates. Expected nanocode-sys edit: 35 s to about 13-15 s (analyzer: 17.9 s wall).
2. **Share the python-capi C archive across machines, and cache it per object (about 110 s per fresh machine; 60-110 s per C-input commit).**
   With empty caches the build script is 112.8 s of a 156 s cold check. bd9cd5994 reuses it per machine; a new
   Hand, a new compiler or any of the 40 commits in 30 days that touched its C inputs still recompiles all ~500
   files. Publish the archive through the remote buildcache (prefetch on box start) and key objects per source
   file so one changed .c recompiles one object.
3. **Drop the nanocode-compression build-dependency from rustpython-unicode and rustpython-doc (about 5 s per affected edit).**
   Same pattern as 1: their build scripts rerun on every compression/sys/djs-libm edit, 4.5 s run + 1.4 s compile on
   the sys-edit critical path (analyzer 4.8 s). Generate the tables once into checked-in or content-keyed output.
4. **Narrow nanocode-sys / nanocode-platform / nanocode-executor fan-out (about 5-8 s per edit after 1 and 3).**
   sys: 61 edits in 30 days, 83 dependents (43 direct); platform: 30 edits, 61 dependents; executor: 22 edits, 62
   dependents; each rebuild is about 128 s cpu on a cold machine. After 1 and 3 the remaining sys-edit path is the
   rustpython chain (rustpython-vm 9.7 s) and the backends; splitting the rarely changing core of sys from what
   changes, so the Python crates depend only on the stable part, takes them off it.
5. **Make pyramid2's nanocode revision bumps incremental (est. 30-40 s per bump).**
   21 of 345 pyramid2 commits in 30 days moved the nanocode git rev; each changes the source of ~90 locked
   nanocode crates in crates/server and so rebuilds all of them (a prior cold server check took 42 s). Patch the
   git source to a local nanocode checkout for development, or have the wrapper cache key on content rather than
   the checkout path.
6. Split nanocode-sim's binary (11.2 s single unit at the end of the cold path, analyzer 5.6 s cold); edits to sim are already 2 s.
7. pyramid2 dev opt-level 2 for core/engine/sim costs 0.6 s per engine edit (2.58 vs 1.99 s); keep it while the simulator's speed needs it.
8. nanocode-code-mode (220 edits, 13 dependents): 3.8 s vs 2.7 s for a leaf; at most about 1 s.

## Reproduce

    cargo check --workspace --all-targets --timings
    nc-build analyze --timings target/cargo-timings/cargo-timing.html --top 25
