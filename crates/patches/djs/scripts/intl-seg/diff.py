#!/usr/bin/env python3
"""Differential test of Intl.Segmenter word granularity against node 22.23.5 (ICU 78.3).

    scripts/intl-seg/diff.py RUNJS DICTDIR [N] [SEED]

DICTDIR holds ICU 78.3's *dict.txt (fetch.sh). Builds sample texts for each script group
(hand-written news-like sentences, then random runs of dictionary words and random
characters, mixed with Latin, digits, punctuation and spaces), segments them with node
($NODE) and with RUNJS (djs-embed's runjs example), and prints mismatches per group
(SHOW=n prints the first n of each).
"""
import json, os, random, subprocess, sys, collections

RUN, DICT = sys.argv[1], sys.argv[2]
N = int(sys.argv[3]) if len(sys.argv) > 3 else 300
SEED = int(sys.argv[4]) if len(sys.argv) > 4 else 1
NODE = os.environ.get("NODE", "node")
here = os.path.dirname(os.path.abspath(__file__))

REAL = json.load(open(os.path.join(here, "samples.json"), encoding="utf-8"))
FILES = {"zh": "cjdict", "ja": "cjdict", "th": "thaidict", "lo": "laodict", "my": "burmesedict", "km": "khmerdict"}
RANGES = {"zh": [(0x4E00, 0x9FFF)], "ja": [(0x3041, 0x3096), (0x30A1, 0x30FA), (0x4E00, 0x9FFF), (0xFF66, 0xFF9F)],
          "th": [(0x0E01, 0x0E3A), (0x0E40, 0x0E4E)], "lo": [(0x0E81, 0x0EDF)], "my": [(0x1000, 0x109F)], "km": [(0x1780, 0x17F9)]}
EXTRA = ["", " ", " ", ", ", "。", "、", "!", "?", "123", "4.5", "abc", "Tempo", "\n", "-", "_", "'", "\u200b", "(", ")", "「", "」", "ー", "々", "\u0301", "\u3099"]


def load(name):
    out = []
    for line in open(os.path.join(DICT, name + ".txt"), encoding="utf-8-sig"):
        line = line.strip()
        if line and not line.startswith("#"):
            out.append(line.split()[0])
    return out


def texts():
    rnd = random.Random(SEED)
    groups = collections.OrderedDict()
    for g in FILES:
        words = load(FILES[g])
        if g == "zh":
            words = [w for w in words if all(0x3040 > ord(c) or ord(c) > 0x30FF for c in w)]
        ts = list(REAL.get(g, []))
        while len(ts) < N:
            k = rnd.random()
            parts = []
            for _ in range(rnd.randint(1, 8)):
                r = rnd.random()
                if r < 0.6:
                    parts.append(rnd.choice(words))
                elif r < 0.8:
                    a, b = rnd.choice(RANGES[g])
                    parts.append("".join(chr(rnd.randint(a, b)) for _ in range(rnd.randint(1, 4))))
                else:
                    parts.append(rnd.choice(EXTRA))
            if k < 0.3:
                parts = [p for p in parts if p not in EXTRA]
            ts.append("".join(parts))
        groups[g] = ts
    mixed = REAL.get("mixed", [])
    allw = [rnd.choice(load(FILES[g])) for g in FILES for _ in range(200)]
    while len(mixed) < N:
        mixed.append("".join(rnd.choice(allw + EXTRA) for _ in range(rnd.randint(2, 10))))
    groups["mixed"] = mixed
    return groups


def src(ts, loc):
    return ("const S=new Intl.Segmenter(%s,{granularity:'word'});for(const t of %s)text(JSON.stringify([...S.segment(t)].map(x=>[x.index,x.isWordLike?1:0])));"
            % (json.dumps(loc), json.dumps(ts, ensure_ascii=False)))


def main():
    groups = texts()
    env = {**os.environ, "NODE_NO_WARNINGS": "1"}
    cases = []
    for g, ts in groups.items():
        for i in range(0, len(ts), 100):
            cases.append((g, ts[i:i + 100], src(ts[i:i + 100], "en" if g == "mixed" else g)))
    ref = []
    for g, ts, s in cases:
        out = subprocess.run([NODE, "-"], input="globalThis.text=v=>process.stdout.write(v+'\\n');" + s, capture_output=True, text=True, env=env).stdout
        ref.append(out.rstrip("\n").split("\n"))
    # One process per batch, as for node: ICU's break engine factory is process-wide.
    outs = []
    for _, _, s in cases:
        p = subprocess.run([RUN], input=json.dumps(s) + "\n", capture_output=True, text=True)
        if p.returncode:
            print("runjs rc", p.returncode, p.stderr[-2000:])
        outs += [l for l in p.stdout.split("\n") if l][:1]
    tot, bad, shown = collections.Counter(), collections.Counter(), collections.Counter()
    show = int(os.environ.get("SHOW", "0"))
    mism = []
    for ci, (g, ts, _) in enumerate(cases):
        got = json.loads(outs[ci]).rstrip("\n").split("\n") if ci < len(outs) else []
        for i, t in enumerate(ts):
            a = ref[ci][i] if i < len(ref[ci]) else "<none>"
            b = got[i] if i < len(got) else "<none>"
            tot[g] += 1
            if a != b:
                bad[g] += 1
                mism.append({"group": g, "text": t, "node": a, "djs": b})
                if shown[g] < show:
                    shown[g] += 1
                    seg = lambda r: "|".join(t[x[0]:(json.loads(r)[k + 1][0] if k + 1 < len(json.loads(r)) else len(t))] + ("*" if x[1] else "") for k, x in enumerate(json.loads(r))) if r.startswith("[") else r
                    print(" ", g, json.dumps(t, ensure_ascii=False), "\n    N", seg(a), "\n    D", seg(b))
    json.dump(mism, open(os.environ.get("MISM", "/tmp/intl-seg-mism.json"), "w"), ensure_ascii=False, indent=0)
    print("total", sum(tot.values()), "mism", len(mism))
    for g in tot:
        print(" ", g, bad[g], "/", tot[g])


if __name__ == "__main__":
    main()
