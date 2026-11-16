# djs vs QuickJS benchmark scoreboard

49 microbenchmarks (`bench/*.js`), each a global `main()` returning a checksum, covering
loops and arithmetic, calls and closures, classes and property access, arrays and their
callbacks, sorting, generators, exceptions, strings, regular expressions, JSON, Map/Set,
Math, typed arrays and allocation-heavy (GC) code.

`run.py` times one call of `main()` in-process in both engines (compile and instantiate are
not timed), runs djs and qjs interleaved A B A B for `-n` pairs after `-w` warm-up pairs,
checks that the checksums match, and reports per bench the median speedup (qjs ms / djs ms,
above 1 means djs is faster) with a 95% bootstrap CI, then the geometric mean and how many
benches have their whole CI at or above 2x, at or above 1x, or below 1x. `--instr` adds
user-space instruction counts (perf, or `--instr-tool callgrind`) as a noise-robust
secondary metric. Use a quiet, pinned host for wall-time numbers.

```sh
# QuickJS reference build
git clone --depth 1 https://github.com/bellard/quickjs output/quickjs
make -C output/quickjs qjs

# djs runner (djs-embed, Code Mode limits off)
cargo build --release --manifest-path scripts/djs-bench/djsrun/Cargo.toml \
    --target-dir output/djs-bench/target

python3 scripts/djs-bench/run.py --djs output/djs-bench/target/release/djsrun \
    --qjs output/quickjs/qjs -n 30 -w 3 --out output/djs-bench/run.json
```
