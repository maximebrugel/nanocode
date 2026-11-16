#!/usr/bin/env python3
"""djs vs QuickJS benchmark scoreboard.

Each bench in bench/*.js defines a global main() that returns a checksum. Both engines
compile the file, then time one call of main() in-process (djsrun: Rust Instant; qjs:
performance.now() in a wrapper written to the output dir), so process spawn and
compilation stay outside the timed region.

Wall time: per bench, -w warm-up pairs, then -n pairs run interleaved A B A B (A = djs,
B = qjs). A pair's speedup is qjs_ms / djs_ms (> 1: djs faster). The table reports the
median speedup with a 95% bootstrap CI of the median, the geometric mean of the medians,
and how many benches have their whole CI at or above 2x, at or above 1x, or below 1x.
Checksums must match between the engines.

Instructions (--instr): perf stat -e instructions:u (or callgrind with
--instr-tool callgrind) over the whole process, minus the same engine running an empty
main(), best of --instr-reps: a noise-robust secondary metric.

Usage:
  cargo build --release --manifest-path scripts/djs-bench/djsrun/Cargo.toml \
      --target-dir output/djs-bench/target
  python3 scripts/djs-bench/run.py --djs output/djs-bench/target/release/djsrun \
      --qjs /path/to/qjs -n 30 -w 3 --out output/djs-bench/run.json [--lock FILE] [--instr]
"""

import argparse
import fcntl
import json
import math
import os
import random
import re
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
QJS_TAIL = """
const __t0 = performance.now();
const __r = main();
const __t1 = performance.now();
print(JSON.stringify({ ms: __t1 - __t0, result: String(__r) }));
"""
EMPTY = "function main(){ return 0; }\n"


def run_json(cmd):
    out = subprocess.run(cmd, capture_output=True, text=True, check=True).stdout
    return json.loads(out.strip().splitlines()[-1])


def same_result(a, b):
    try:
        x, y = float(a), float(b)
        return x == y or abs(x - y) <= 1e-9 * max(abs(x), abs(y))
    except ValueError:
        return a == b


def median(xs):
    s = sorted(xs)
    n = len(s)
    return s[n // 2] if n % 2 else 0.5 * (s[n // 2 - 1] + s[n // 2])


def boot_ci(xs, reps=4000, seed=1):
    rng = random.Random(seed)
    n = len(xs)
    meds = sorted(median([xs[rng.randrange(n)] for _ in range(n)]) for _ in range(reps))
    return meds[int(0.025 * reps)], meds[int(0.975 * reps) - 1]


def loadavg():
    return os.getloadavg()[0]


def instr(cmd, tool, reps):
    best = None
    for _ in range(reps):
        n = None
        if tool == "perf":
            r = subprocess.run(["perf", "stat", "-x,", "-e", "instructions:u"] + cmd,
                               capture_output=True, text=True, check=True)
            for line in r.stderr.splitlines():
                f = line.split(",")
                if len(f) > 2 and f[2].startswith("instructions") and f[0].isdigit():
                    n = int(f[0])
        else:
            r = subprocess.run(["valgrind", "--tool=callgrind",
                                "--callgrind-out-file=/dev/null"] + cmd,
                               capture_output=True, text=True, check=True)
            m = re.search(r"Collected\s*:\s*(\d+)", r.stderr)
            n = int(m.group(1)) if m else None
        if n is None:
            raise RuntimeError("no instruction count for " + " ".join(cmd))
        best = n if best is None else min(best, n)
    return best


class Lock:
    def __init__(self, path):
        self.path = path
        self.fd = None

    def __enter__(self):
        if self.path:
            self.fd = open(self.path, "a")
            fcntl.flock(self.fd, fcntl.LOCK_EX)
        return self

    def __exit__(self, *exc):
        if self.fd:
            fcntl.flock(self.fd, fcntl.LOCK_UN)
            self.fd.close()


def wait_load(max_load, max_wait):
    t0 = time.time()
    while max_load > 0 and loadavg() >= max_load and time.time() - t0 < max_wait:
        time.sleep(15)


def summarize(rows, wall, ins):
    s = {"benches": len(rows), "mismatches": [r["bench"] for r in rows if not r["match"]]}
    if wall and rows:
        meds = [r["speedup_med"] for r in rows]
        s.update(
            geomean=math.exp(sum(map(math.log, meds)) / len(meds)),
            ci_ge_2x=sum(r["ci_lo"] >= 2 for r in rows),
            ci_ge_1x=sum(r["ci_lo"] >= 1 for r in rows),
            ci_lt_1x=sum(r["ci_hi"] < 1 for r in rows),
            ci_straddles_1x=sum(r["ci_lo"] < 1 <= r["ci_hi"] for r in rows),
            worst5=[r["bench"] for r in sorted(rows, key=lambda r: r["speedup_med"])[:5]],
            max_load=max(r["load_min_max"][1] for r in rows),
        )
    if ins and rows:
        im = [r["instr_speedup"] for r in rows]
        s.update(
            instr_geomean=math.exp(sum(map(math.log, im)) / len(im)),
            instr_ge_2x=sum(x >= 2 for x in im),
            instr_ge_1x=sum(x >= 1 for x in im),
            instr_lt_1x=sum(x < 1 for x in im),
            instr_worst5=[r["bench"] for r in sorted(rows, key=lambda r: r["instr_speedup"])[:5]],
        )
    return s


def main():
    ap = argparse.ArgumentParser(description="djs vs QuickJS benchmark scoreboard")
    ap.add_argument("--djs", required=True)
    ap.add_argument("--qjs", required=True)
    ap.add_argument("-n", type=int, default=30)
    ap.add_argument("-w", type=int, default=3)
    ap.add_argument("--only", default="", help="comma-separated bench names")
    ap.add_argument("--out", default="output/djs-bench/run.json")
    ap.add_argument("--lock", default="", help="flock this file around each bench")
    ap.add_argument("--max-load", type=float, default=0.0,
                    help="before each bench, wait up to --max-wait s for 1-min loadavg below this")
    ap.add_argument("--max-wait", type=float, default=600.0)
    ap.add_argument("--no-wall", action="store_true")
    ap.add_argument("--instr", action="store_true")
    ap.add_argument("--instr-tool", default="perf", choices=["perf", "callgrind"])
    ap.add_argument("--instr-reps", type=int, default=1)
    a = ap.parse_args()

    out = Path(a.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    wrap = out.parent / "qjs-wrapped"
    wrap.mkdir(exist_ok=True)
    benches = sorted((HERE / "bench").glob("*.js"))
    if a.only:
        keep = set(a.only.split(","))
        benches = [p for p in benches if p.stem in keep]
    empty = out.parent / "empty.js"
    empty.write_text(EMPTY)
    (wrap / "empty.js").write_text(EMPTY + QJS_TAIL)

    base = {}
    if a.instr:
        base["djs"] = instr([a.djs, str(empty)], a.instr_tool, a.instr_reps)
        base["qjs"] = instr([a.qjs, str(wrap / "empty.js")], a.instr_tool, a.instr_reps)

    meta = {"djs": a.djs, "qjs": a.qjs, "n": a.n, "w": a.w, "nproc": os.cpu_count(),
            "instr_tool": a.instr_tool if a.instr else None, "instr_base": base}
    rows = []
    for p in benches:
        q = wrap / p.name
        q.write_text(p.read_text() + QJS_TAIL)
        A = [a.djs, str(p)]
        B = [a.qjs, str(q)]
        row = {"bench": p.stem}
        wait_load(a.max_load, a.max_wait)
        with Lock(a.lock):
            l0 = loadavg()
            ra, rb = run_json(A), run_json(B)
            row["result_djs"], row["result_qjs"] = ra["result"], rb["result"]
            row["match"] = same_result(ra["result"], rb["result"])
            if not a.no_wall:
                for _ in range(a.w):
                    run_json(A)
                    run_json(B)
                da, db, sp = [], [], []
                for _ in range(a.n):
                    x = run_json(A)["ms"]
                    y = run_json(B)["ms"]
                    da.append(x)
                    db.append(y)
                    sp.append(y / x)
                lo, hi = boot_ci(sp)
                row.update(djs_ms=da, qjs_ms=db, speedups=sp, djs_ms_med=median(da),
                           qjs_ms_med=median(db), speedup_med=median(sp), ci_lo=lo, ci_hi=hi)
            if a.instr:
                ia = instr(A, a.instr_tool, a.instr_reps) - base["djs"]
                ib = instr(B, a.instr_tool, a.instr_reps) - base["qjs"]
                row.update(djs_instr=ia, qjs_instr=ib, instr_speedup=ib / ia)
            l1 = loadavg()
        row["load_min_max"] = [min(l0, l1), max(l0, l1)]
        rows.append(row)
        msg = f"{p.stem:20s} match={row['match']}"
        if "speedup_med" in row:
            msg += (f" djs {row['djs_ms_med']:8.2f} ms qjs {row['qjs_ms_med']:8.2f} ms"
                    f" x{row['speedup_med']:.2f} [{row['ci_lo']:.2f}, {row['ci_hi']:.2f}]")
        if "instr_speedup" in row:
            msg += f" instr x{row['instr_speedup']:.2f}"
        print(msg + f" load {row['load_min_max'][0]:.0f}-{row['load_min_max'][1]:.0f}", flush=True)
        summary = summarize(rows, not a.no_wall, a.instr)
        out.write_text(json.dumps({"meta": meta, "summary": summary, "rows": rows}, indent=1))

    summary = summarize(rows, not a.no_wall, a.instr)
    print(json.dumps(summary, indent=1))
    return 1 if summary["mismatches"] else 0


if __name__ == "__main__":
    sys.exit(main())
