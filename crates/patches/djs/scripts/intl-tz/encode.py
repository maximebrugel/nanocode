#!/usr/bin/env python3
"""Encode the probed zone history (probe.mjs output) into djs-core's intl/tzdata.rs.
usage: encode.py PROBE.json LOC.json[,LOC.json...] INTL_DIR TZDATA_RS [DEBUG.json]
(LOC.json is probe-loc.mjs output, its shards joined by commas; TZDATA_RS is head.rs, the decoder, then the data)

Each zone gets a few period types (UTC offset, dst flag, names tuple) and the transitions between them,
from 1800 to the point where the zone's current POSIX rule (ZONES' second field) and current names
reproduce every later transition up to 2101 (or to 2101 when they never do).

Names tuple 0 means the zone's current ZONES names (picked by dst, with EN_GENERIC_ALT in the repeated
hour); tuple n >= 1 is TZ_NAMES[n - 1]: string ids for short, long, shortGeneric, longGeneric, and the
two generic names in the repeated hour before an offset drop. String id 0 is the short GMT format of
the type's offset (GMT+5:21:10), 1 the long one (GMT+05:21:10), n >= 2 the TZ_STR string n - 2.

Every other DateTimeFormat locale reads a tuple's names through its metazone: the tuple names a reference
zone R whose current names (DateLocale.zones) are that metazone's in every locale, and for each element
(short, long, shortGeneric, longGeneric, the repeated-hour pair) a source: 0..7 R's zones slot, 8 the
locale's GMT format, 9 the zone's own current names (as if no history), 10 the en string. Locales whose
names differ from that (location formats, a region's golden zone, metazones no zone has today) override
an element with another source or a string (TZ_STR id + 11). Sources are chosen to match the probe at
each period's first and last second and in its repeated hour.

Zone stream (TZ_HISTORY at TZ_INDEX[zone]), all varints:
  ntypes; per type: off (zz(off/60) << 1, or zz(off) << 1 | 1 when not whole minutes), tuple << 1 | dst
  cycle c; npoints; per point i (the first period starts at 1800-01-01T00:00Z with type 0):
    d_i = (t_i - t_{i-1}) - (d_{i-c} when i >= c, else 0), in the largest unit u of 3600/60/1 dividing it
    (zz(d_i / unit) << 2 | u) << 1 | same, where same means the type equals point i-c's type;
    when not same, a varint type follows. Type ntypes marks the start of the POSIX rule tail.
"""
import json, os, re, sys, datetime

probe = json.load(open(sys.argv[1]))
LN = {}
for f in sys.argv[2].split(","):
    LN.update(json.load(open(f)))
sys.argv[2:3] = []
D = sys.argv[2]
data = open(D + "/data.rs").read()
loc = open(D + "/date/locales.rs").read()
a = data.index("pub(super) static ZONES")
ZONES = [(m[0], m[1], [int(x) for x in m[2].split(",")]) for m in re.findall(r'^    \("([^"]+)", "([^"]*)", \[([0-9, ]+)\]\)', data[a:data.index("];", a)], re.M)]
a = data.index("pub(super) static ZONE_NAMES")
ZN = [re.sub(r"\\u\{([0-9a-fA-F]+)\}", lambda m: chr(int(m[1], 16)), x) for x in re.findall(r'"([^"]*)"', data[data.index("= &[", a):data.index("];", a)])]
TEXT = re.search(r'DTF_STRINGS_TEXT: &str = "([^"]*)"', loc)[1].encode()
ENDS = [int(x) for x in re.search(r"DTF_STRINGS_ENDS: &\[u32\] = &\[([0-9, ]*)\]", loc)[1].split(",")]
dtf = lambda i: TEXT[(ENDS[i - 1] if i else 0):ENDS[i]].decode()
ALT = {int(m[0]): [dtf(int(m[1])), dtf(int(m[2]))] for m in re.findall(r"\((\d+), (\d+), (\d+)\)", re.search(r"EN_GENERIC_ALT: &\[\(u16, u16, u16\)\] = &\[([^\]]*)\]", loc)[1])}
LOCALES = re.findall(r'tags: &\["([^"]+)"', loc)  # DATE_LOCALES' first tags, as probe-loc.mjs reads them
LTAB = re.findall(r"zones: &([A-Z_]+)", loc)
LGMT = [(m[0], m[1]) for m in re.findall(r'gmt: \["([^"]*)", "([^"]*)"\]', loc)]
assert len(LTAB) == len(LGMT) == len(LOCALES)
LZ = [[int(x) for x in re.search(r"static " + t + r": \[u16; \d+\] = \[([0-9, ]*)\]", loc)[1].split(",")] for t in LTAB]
NZ = len(LZ[0]) // 8
def ltoday(li, R, slot):
    v = LZ[li][R * 8 + slot]
    if v == 65535 and slot >= 6:
        v = LZ[li][R * 8 + slot - 2]
    return None if v == 65535 else dtf(v)
def lgmt(li, off, long, g=None):
    """date.rs locale_offset: the name a locale's GMT format gives the offset."""
    g, minus = (LGMT[li][0] if g is None else g), LGMT[li][1]
    if g.endswith("\u200e"):
        # he isolates the offset with LRMs: GMT\u200e+5\u200e, but GMT-5\u200e (GMT-05:00\u200e\u200e long)
        plain = lgmt(li, off, long, g[:-1])
        head, tail = plain[:len(g) - 1], plain[len(g) - 1:]
        return head + tail + "\u200e" + ("\u200e" if long else "") if off < 0 else head + "\u200e" + tail + "\u200e"
    sg = minus if off < 0 else "+"
    h, m, sec = abs(off) // 3600, abs(off) // 60 % 60, abs(off) % 60
    secs = f":{sec:02}" if sec else ""
    if long:
        return f"{g}{sg}{h:02}:{m:02}{secs}"
    return f"{g}{sg}{h}:{m:02}{secs}" if m or sec else f"{g}{sg}{h}"
LIDX = []
for li in range(len(LOCALES)):
    d = {}
    for R in range(len(LZ[li]) // 8):
        for sl in range(8):
            d.setdefault(ltoday(li, R, sl), set()).add(R)
    LIDX.append(d)
US = {"CST6CDT": "CST6CDT,M3.2.0,M11.1.0", "EST5EDT": "EST5EDT,M3.2.0,M11.1.0", "MST7MDT": "MST7MDT,M3.2.0,M11.1.0", "PST8PDT": "PST8PDT,M3.2.0,M11.1.0"}
order = [z[0] for z in ZONES] + list(US) + ["Etc/Greenwich"]
entry = {z[0]: (i, z) for i, z in enumerate(ZONES)}
entry["Etc/Greenwich"] = entry["Africa/Abidjan"]
START, END = -5364662400, 4133980800

def gmt(off, long):
    s = "-" if off < 0 else "+"
    h, m, sec = abs(off) // 3600, abs(off) // 60 % 60, abs(off) % 60
    if long:
        return f"GMT{s}{h:02}:{m:02}" + (f":{sec:02}" if sec else "")
    return f"GMT{s}{h}" + (f":{m:02}" if m or sec else "") + (f":{sec:02}" if sec else "")

# ---- POSIX rule, read as date.rs reads it ----
def parse_posix(s):
    i = 0
    def name():
        nonlocal i
        if i < len(s) and s[i] == "<":
            i = s.index(">", i) + 1
        else:
            while i < len(s) and s[i].isalpha():
                i += 1
    def offset():
        nonlocal i
        sign = 1
        if i < len(s) and s[i] in "+-":
            sign = -1 if s[i] == "-" else 1
            i += 1
        parts = [0, 0, 0]; k = 0
        while True:
            j = i
            while i < len(s) and s[i].isdigit():
                i += 1
            if j == i:
                return None if k == 0 else sign * (parts[0] * 3600 + parts[1] * 60 + parts[2])
            parts[k] = int(s[j:i]); k += 1
            if k < 3 and i < len(s) and s[i] == ":":
                i += 1
            else:
                return sign * (parts[0] * 3600 + parts[1] * 60 + parts[2])
    def trans():
        nonlocal i
        if s[i] == "M":
            i += 1
            m = re.match(r"(\d+)\.(\d+)\.(\d+)", s[i:]); i += m.end()
            day = ("M", int(m[1]), int(m[2]), int(m[3]))
        elif s[i] == "J":
            i += 1
            m = re.match(r"\d+", s[i:]); i += m.end(); day = ("J", int(m[0]))
        else:
            m = re.match(r"\d+", s[i:]); i += m.end(); day = ("Z", int(m[0]))
        secs = 7200
        if i < len(s) and s[i] == "/":
            i += 1; secs = offset()
        return day, secs
    name(); std = -offset()
    if i >= len(s):
        return std, None
    name()
    dst = std + 3600 if s[i] == "," else -offset()
    i += 1
    a = trans(); i += 1; b = trans()
    return std, (dst, a, b)

def leap(y):
    return (y % 4 == 0 and y % 100 != 0) or y % 400 == 0

def days_from_civil(y, m, d):
    return (datetime.date(y, m, d) - datetime.date(1970, 1, 1)).days

def trans_local(t, y):
    day, secs = t
    if day[0] == "M":
        _, m, w, wd = day
        first = days_from_civil(y, m, 1)
        d = (wd + 7 - (first + 4) % 7) % 7 + 1 + (w - 1) * 7
        ln = [31, 29 if leap(y) else 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31][m - 1]
        while d > ln:
            d -= 7
        dd = first + d - 1
    elif day[0] == "J":
        dd = days_from_civil(y, 1, 1) + day[1] - 1 + (1 if leap(y) and day[1] >= 60 else 0)
    else:
        dd = days_from_civil(y, 1, 1) + day[1]
    return dd * 86400 + secs

def year_of(t):
    return (datetime.datetime(1970, 1, 1) + datetime.timedelta(days=t // 86400)).year

def rule_at(r, t):
    std, d = r
    if not d:
        return std, False
    dst, a, b = d
    y = year_of(t + std)
    start, end = trans_local(a, y) - std, trans_local(b, y) - dst
    ind = (start <= t < end) if start < end else not (end <= t < start)
    return (dst if ind else std), ind

def rule_changes(r, t0, t1):
    """UTC instants in (t0, t1) where the rule's offset changes (exact, from each year's two transitions)."""
    std, d = r
    if not d:
        return []
    dst, a, b = d
    out = []
    for y in range(year_of(t0) - 1, year_of(t1) + 2):
        for x in (trans_local(a, y) - std, trans_local(b, y) - dst):
            if t0 < x < t1 and rule_at(r, x)[0] != rule_at(r, x - 1)[0]:
                out.append(x)
    return out

# ---- types and tuples ----
STR = {}
def sid(s, off):
    if s == gmt(off, False):
        return 0
    if s == gmt(off, True):
        return 1
    if s not in STR:
        STR[s] = len(STR)
    return STR[s] + 2
TUP = {}
def cur_dst(z, names, alt):
    if z not in entry:
        return None
    zi, e = entry[z]
    if alt is not None and alt != ALT.get(zi, names[2:4]):
        return None
    for d in (0, 1):
        if names == [ZN[e[2][d]], ZN[e[2][2 + d]], ZN[e[2][4]], ZN[e[2][5]]]:
            return d
    return None


OWN_SPEC = (0, (9,) * 6, ())
def choose_spec(z, off, dst, en, smp):
    """The locale names spec (R + 1 or 0, six element codes, overrides) for a type with these samples, and
    the string overrides that would match more of them: [(li, e, kind, string, gain)], kind "s" for the
    whole name and "p" for a suffix to the element's default source.

    A locale reads an element from a source only when that matches every sample the zone's own current
    names match (what djs showed before it had historical names), so no choice loses a correct name; of
    those, the one matching the most samples, weighted to periods since 1970."""
    zi = entry[z][0] if z in entry else None
    exp = {}
    for i, k, ov in smp:
        w = 3 if probe[z][i][0] >= 0 else 1
        for li, L in enumerate(LOCALES):
            v = LN[z][L][i][k]
            if v is None:
                continue
            for e, x in ((0, v[0]), (1, v[1]), (5 if ov else 3, v[3]), (4 if ov else 2, v[2])):
                exp.setdefault((e, li), []).append((w, x))
    def val(li, e, code, R):
        lg = e in (1, 3, 5)
        if code < 8:
            v = ltoday(li, R, code) if R is not None else None
        elif code == 8:
            v = None
        elif code == 9:
            v = ltoday(li, zi, [dst, 2 + dst, 4, 5, 6, 7][e]) if zi is not None else None
        else:
            return en[e]
        return lgmt(li, off, lg) if v is None else v
    cnt = {}
    for (e, li), X in exp.items():
        for x in {x for _, x in X}:
            for R in LIDX[li].get(x, ()):
                cnt[R] = cnt.get(R, 0) + 1
    cands = [None] + sorted(cnt, key=lambda R: (-cnt[R], R))[:24]
    order = [9] + [c for c in range(11) if c != 9]
    best = None
    for R in cands:
        codes, ovr, strs, score = [], [], [], 0
        for e in range(6):
            per_l = {}
            for li in range(len(LOCALES)):
                X = exp.get((e, li))
                if not X:
                    continue
                vals = [val(li, e, code, R) for code in range(11)]
                ms = [frozenset(j for j, (_, x) in enumerate(X) if x == vals[code]) for code in range(11)]
                wt = [sum(X[j][0] for j in m) for m in ms]
                # the en name (10) only for English locales: elsewhere it matches by accident at a sample
                valid = [c for c in order if ms[c] >= ms[9] and (c != 10 or LOCALES[li].startswith("en"))]
                top = max(valid, key=lambda c: wt[c])
                per_l[li] = (X, vals, ms, wt, valid, top)
            if not per_l:
                codes.append(9 if e < 4 else codes[e - 2] + 2 if codes[e - 2] in (4, 5) else codes[e - 2])
                continue
            dflt = max(order, key=lambda c: sum(1 for (_, _, _, wt, valid, top) in per_l.values() if c in valid and wt[c] == wt[top]))
            codes.append(dflt)
            for li, (X, vals, ms, wt, valid, top) in per_l.items():
                use = dflt if dflt in valid and wt[dflt] == wt[top] else top
                if use != dflt:
                    ovr.append((li, e, use))
                score += wt[use]
                tot = {}
                for w, x in X:
                    tot[x] = tot.get(x, 0) + w
                x = max(sorted(tot), key=tot.get)
                xs = frozenset(j for j, (_, y) in enumerate(X) if y == x)
                if x != vals[use] and xs >= ms[9] and tot[x] > wt[use]:
                    base = vals[dflt]
                    if use == dflt and x.startswith(base) and x != base:
                        strs.append((li, e, "p", x[len(base):], tot[x] - wt[use]))
                    else:
                        strs.append((li, e, "s", x, tot[x] - wt[use]))
        key = (score + sum(g for *_, g in strs), score, -len(ovr))
        if best is None or key > best[0]:
            best = (key, (0 if R is None else R + 1, tuple(codes), tuple(ovr)), strs)
    _, sp, strs = best
    if sp[0] and not any(c < 8 for c in sp[1]) and not any(o[2] < 8 for o in sp[2]):
        sp = (0,) + sp[1:]
    return sp, strs

PENDING = {}
out_z = {}
stats = dict(periods=0, types=0, kept=0, folded=0)
for z in order:
    P = probe[z]
    # fold each repeated-hour period into the period before it, as that period's alt generic names
    per = []
    for i, p in enumerate(P):
        t, off, dst, names = p
        nxt = P[i + 1] if i + 1 < len(P) else None
        if per and off == per[-1][1] and nxt and nxt[1] < off and nxt[0] - t == off - nxt[1] and names[:2] == per[-1][3][:2] and per[-1][4] is None:
            per[-1][4] = names[2:4]
            per[-1][5] += [(i, 0, True), (i, 1, True)]
            stats["folded"] += 1
            continue
        drop = nxt is not None and nxt[1] < off
        per.append([t, off, dst, names, None, [(i, 0, False), (i, 1, drop), (i, 2, True)]])
    # a period ending in an offset drop without an alt shows its own generic names in the repeated hour
    # (periods not ending in a drop have no repeated hour: their alt is None, matching anything)
    for i, p in enumerate(per):
        if p[4] is None and i + 1 < len(per) and per[i + 1][1] < p[1]:
            p[4] = p[3][2:4]
    r = parse_posix(entry[z][1][1] if z in entry else US[z])
    def conforms(j):
        t, off, _, names, alt = per[j][:5]
        ro, rd = rule_at(r, t)
        cd = cur_dst(z, names, alt)
        end = per[j + 1][0] if j + 1 < len(per) else END
        return ro == off and cd == int(rd) and not rule_changes(r, t, end) and (j + 1 == len(per) or rule_at(r, end)[0] != rule_at(r, end - 1)[0] or per[j + 1][1] == off)
    k = len(per)
    while k > 1 and conforms(k - 1):
        k -= 1
    explicit = per[:k]
    rule_from = per[k][0] if k < len(per) else None
    types, trans = [], []
    groups = {}
    def sig(smp):
        return tuple(tuple(LN[z][L][i][k] or ()) for i, k, _ in smp for L in LOCALES)
    for t, off, dst, names, alt, smp in explicit:
        cd = cur_dst(z, names, alt)
        en = tuple(names + (alt or names[2:4]))
        g = groups.setdefault((off, dst if cd is None else cd, cd is not None, en, sig(smp)), [])
        g += smp
    specs = {k: choose_spec(z, k[0], k[1], k[3], v) for k, v in groups.items()}
    for t, off, dst, names, alt, smp in explicit:
        cd = cur_dst(z, names, alt)
        en = tuple(names + (alt or names[2:4]))
        k = (off, dst if cd is None else cd, cd is not None, en, sig(smp))
        sp, cand = specs[k]
        if cd is not None and sp == OWN_SPEC and not cand:
            ty = (off, cd, 0)
        else:
            pk = (z, k)
            if pk not in PENDING:
                PENDING[pk] = (tuple(sid(x, off) for x in en), sp, cand, off)
            ty = (off, k[1], pk)
        if ty not in types:
            types.append(ty)
        trans.append((t, types.index(ty)))
    out_z[z] = (types, trans, rule_from)
    stats["periods"] += len(P); stats["kept"] += len(trans); stats["types"] += len(types)

def varint(n, b):
    assert n >= 0
    while True:
        x = n & 0x7F; n >>= 7
        if n:
            b.append(x | 0x80)
        else:
            b.append(x); return

def zz(n):
    return n * 2 if n >= 0 else -n * 2 - 1

def stream(types, pts, c):
    b = bytearray()
    varint(len(types), b)
    for off, dst, tup in types:
        varint(zz(off // 60) << 1 if off % 60 == 0 else zz(off) << 1 | 1, b)
        varint(tup << 1 | dst, b)
    varint(c, b); varint(len(pts), b)
    prev, ds = START, []
    for i, (t, ty) in enumerate(pts):
        dl = t - prev; prev = t; ds.append(dl)
        d = dl - (ds[i - c] if i >= c else 0)
        u = 0 if d % 3600 == 0 else 1 if d % 60 == 0 else 2
        same = i >= c and pts[i - c][1] == ty
        varint((zz(d // (3600, 60, 1)[u]) << 2 | u) << 1 | same, b)
        if not same:
            varint(ty, b)
    return bytes(b)

# Strings compete for STRING_BUDGET bytes by the samples they add per byte (a string's bytes plus about
# three per use); a type then reads the chosen ones as overrides.
STRING_BUDGET = int(os.environ.get("TZ_STRING_BUDGET", "60000"))
gain, uses = {}, {}
for pk, (en_ids, sp, cand, off) in PENDING.items():
    for li, e, kind, x, g in cand:
        gain[x] = gain.get(x, 0) + g
        uses[x] = uses.get(x, 0) + 1
cost = lambda x: (0 if x in STR else len(x.encode()) + 1) + 3 * uses[x]
chosen, spent = set(), 0
for x in sorted(gain, key=lambda x: (-gain[x] / cost(x), x)):
    if spent + cost(x) <= STRING_BUDGET:
        chosen.add(x); spent += cost(x)
stats["strings"] = f"{len(chosen)} of {len(gain)} chosen, {spent} bytes, gain {sum(gain[x] for x in chosen)} of {sum(gain.values())}"
FINAL = {}
for pk, (en_ids, sp, cand, off) in PENDING.items():
    ovr = dict(((li, e), c) for li, e, c in sp[2])
    for li, e, kind, x, g in cand:
        if x in chosen:
            ovr[(li, e)] = 11 + 2 * sid(x, off) + (kind == "p")
    spec = (sp[0], sp[1], tuple((li, e, c) for (li, e), c in sorted(ovr.items())))
    tup = (en_ids, spec)
    if tup not in TUP:
        TUP[tup] = len(TUP) + 1
    FINAL[pk] = TUP[tup]
for z in order:
    types, trans, rule_from = out_z[z]
    ty = [(o, d, FINAL[x] if isinstance(x, tuple) else x) for o, d, x in types]
    uniq = list(dict.fromkeys(ty))
    out_z[z] = (uniq, [(t, uniq.index(ty[j])) for t, j in trans], rule_from)

blob = bytearray(); index = []; seen = {}
for z in order:
    types, trans, rule_from = out_z[z]
    assert trans[0] == (START, 0), (z, trans[0])
    pts = trans[1:] + ([(rule_from, len(types))] if rule_from is not None else [])
    key = min((stream(types, pts, c) for c in range(1, 9)), key=len)
    if key not in seen:
        seen[key] = len(blob); blob += key
    index.append(seen[key])
strs = list(STR)
tups = sorted(TUP, key=TUP.get)
sys.stderr.write(json.dumps(stats) + f" blob={len(blob)} strs={len(strs)} tuples={len(tups)}\n")
if len(sys.argv) > 4:
    json.dump({z: list(out_z[z]) for z in order}, open(sys.argv[4], "w"))
    import pickle
    pickle.dump((TUP, STR, out_z), open(sys.argv[4] + ".pkl", "wb"))

# ---- packing: strings get a small substring dictionary (control chars \u{1}..), tuples a varint stream ----
def build_dict(strs, n=24):
    words, cur = [], list(strs)
    for code in range(1, n + 1):
        cnt = {}
        for s in cur:
            seen = set()
            for i in range(len(s)):
                for j in range(i + 3, min(len(s), i + 24) + 1):
                    w = s[i:j]
                    if any(ord(c) < 32 for c in w) or w in seen:
                        continue
                    seen.add(w)
                    cnt[w] = cnt.get(w, 0) + 1
        # occurrences counted once per string: a fair estimate for these short names
        best = max(cnt, key=lambda w: cnt[w] * (len(w.encode()) - 1) - len(w.encode()), default=None)
        if not best or cnt[best] * (len(best.encode()) - 1) - len(best.encode()) <= 0:
            break
        words.append(best)
        cur = [s.replace(best, chr(code)) for s in cur]
    return words, cur
words, packed = build_dict(strs)
def unpack(s):
    return "".join(words[ord(c) - 1] if ord(c) <= len(words) else c for c in s)
assert [unpack(s) for s in packed] == strs
text = "".join(packed)
# string k is TZ_STR_LEN[k] bytes at TZ_STR_AT[k / 32] plus the lengths of the strings before it in its block
lens, at, n = [], [], 0
for k, s in enumerate(packed):
    if k % 32 == 0:
        at.append(n)
    assert len(s.encode()) < 256
    lens.append(len(s.encode())); n += len(s.encode())
assert n < 1 << 32
tb, tidx = bytearray(), []
for t, sp in tups:
    tidx.append(len(tb))
    h = (t[0] == 0) | (t[1] == 1) << 1 | (t[3] == t[2]) << 2 | (t[4:6] == t[2:4]) << 3
    tb.append(h)
    for v in ([] if h & 1 else [t[0]]) + ([] if h & 2 else [t[1]]) + [t[2]] + ([] if h & 4 else [t[3]]) + ([] if h & 8 else [t[4], t[5]]):
        varint(v, tb)
    varint(sp[0], tb)
    c = sp[1]
    tb += bytes([c[0] | c[1] << 4, c[2] | c[3] << 4, c[4] | c[5] << 4])
    varint(len(sp[2]), tb)
    for li, e, code in sp[2]:
        varint(li * 6 + e, tb); varint(code, tb)
assert len(tb) < 1 << 24
def rs_str(s):
    return '"' + "".join("\\u{%x}" % ord(c) if ord(c) < 32 else "\\\"" if c == '"' else "\\\\" if c == "\\" else c for c in s) + '"'
sys.stderr.write(f"packed strbytes={n} words={len(words)} tuplebytes={len(tb)}\n")
o = [
    "// Generated by scripts/intl-tz/encode.py from the reference node's zone history (probe.mjs); do not edit.",
    "// The format is described in encode.py.",
    "",
    
    f"pub(super) static TZ_INDEX: [u32; {len(index)}] = [{','.join(map(str, index))}];",
    f"pub(super) static TZ_HISTORY: [u8; {len(blob)}] = [{','.join(map(str, blob))}];",
    "/// Names tuple n >= 1 starts at TZ_TUPLES[TZ_TUPLE_AT[n - 1]]: a flags byte (bit 0: short is id 0, bit 1: long",
    "/// is id 1, bit 2: longGeneric equals shortGeneric, bit 3: the repeated-hour pair equals the generic pair),",
    "/// then varint string ids for short, long, shortGeneric, longGeneric and the repeated-hour pair, skipping flagged ones.",
    f"pub(super) static TZ_TUPLE_AT: [{'u16' if len(tb) < 65536 else 'u32'}; {len(tidx)}] = [{','.join(map(str, tidx))}];",
    f"pub(super) static TZ_TUPLES: [u8; {len(tb)}] = [{','.join(map(str, tb))}];",
    "/// String i is TZ_STR_LEN[i] bytes of TZ_STR_TEXT (see TZ_STR_AT), where char k < 32 stands for TZ_WORDS[k - 1].",
    f"pub(super) static TZ_WORDS: [&str; {len(words)}] = [{','.join(rs_str(w) for w in words)}];",
    f"pub(super) static TZ_STR_TEXT: &str = {rs_str(text)};",
    f"pub(super) static TZ_STR_AT: [u32; {len(at)}] = [{','.join(map(str, at))}];",
    "/// DATE_LOCALES' first tags when the data was encoded: locale_src takes a locale's index here.",
    f"pub(super) static TZ_LOCALES: [&str; {len(LOCALES)}] = [{', '.join(chr(34) + x + chr(34) for x in LOCALES)}];",
    f"pub(super) static TZ_STR_LEN: [u8; {len(lens)}] = [{','.join(map(str, lens))}];",
]
o.insert(3, f"pub(super) const TZ_MAX_TYPES: usize = {max(len(v[0]) for v in out_z.values())};")
head = open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "head.rs")).read()
open(sys.argv[3], "w").write(head + "".join("#[rustfmt::skip]\n" + l + "\n" for l in o if l and not l.startswith("//")))
