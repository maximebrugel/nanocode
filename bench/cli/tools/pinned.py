# Median, min, max, p90 and spread per row of a pinned.sh suite.jsonl: pinned.py suite.jsonl [warmup rounds to drop]
# ru_maxrss is the measure to compare. The own_peak fields are the command's smaps_rollup at the sampler's highest RSS
# reading (2 ms period), so they miss short peaks and serve only to split anonymous from file-backed memory.
import json, sys

rows = [json.loads(line) for line in open(sys.argv[1])]
skip = int(sys.argv[2]) if len(sys.argv) > 2 else 0
fields = [("max_rss_mb", "ru_maxrss MiB"), ("own_peak_rss_mb", "own Rss MiB"), ("own_peak_pss_mb", "own Pss MiB"), ("own_peak_anon_mb", "own Anon MiB"), ("own_peak_file_mb", "own file MiB"), ("minflt", "minor faults"), ("wall_ms", "wall ms")]
counts = ("minflt", "wall_ms")
groups = {}
for r in rows:
    if r.get("harness") == "true" or r.get("error") or r.get("round", 0) < skip:
        continue
    key = r["part"] + ("/" + r["tool_mode"] if r.get("tool_mode") else "") + ("/" + str(r["steps"]) if r.get("steps") else "")
    groups.setdefault(key, []).append(r)

def stat(values):
    v = sorted(values)
    n = len(v)
    med = v[n // 2] if n % 2 else (v[n // 2 - 1] + v[n // 2]) / 2
    return med, v[0], v[-1], v[min(n - 1, -(-9 * n // 10) - 1)]

for key, rs in groups.items():
    print(f"{key} (n={len(rs)})")
    print(f"  {'metric':16} {'median':>9} {'min':>9} {'max':>9} {'p90':>9} {'spread':>8}")
    for field, name in fields:
        v = [r[field] for r in rs if r.get(field) is not None]
        if not v:
            continue
        med, lo, hi, p90 = stat(v)
        fmt = "{:9.0f}" if field in counts else "{:9.3f}"
        print(f"  {name:16} " + " ".join(fmt.format(x) for x in (med, lo, hi, p90)) + " " + fmt.format(hi - lo)[1:])
errors = [r for r in rows if r.get("error")]
if errors:
    print(f"errors: {len(errors)}: {errors[0]['error'][:200]}")
