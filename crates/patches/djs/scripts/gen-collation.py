#!/usr/bin/env python3
"""Generates crates/djs-core/src/builtins/intl/ducet.rs, the CLDR root collation table
(ICU's FractionalUCA_SHORT.txt, the data ICU's root collator is built from) for
Intl.Collator and String.prototype.localeCompare.

usage: gen-collation.py FractionalUCA_SHORT.txt crates/djs-core/src/builtins/intl/han.bin \
    > crates/djs-core/src/builtins/intl/ducet.rs
Fetch the file for the CLDR release Node's ICU uses, e.g.
https://raw.githubusercontent.com/unicode-org/cldr/release-48/common/uca/FractionalUCA_SHORT.txt
"""
import re, sys, unicodedata

src = open(sys.argv[1], encoding='utf-8').read().split('\n')
ent = []
prefix = []
unified = []
version = ''
for l in src:
    m = re.match(r'\[UCA version = (.*)\]', l)
    if m:
        version = m.group(1)
    m = re.match(r'\[Unified_Ideograph (.*)\]', l)
    if m:
        for r in m.group(1).split():
            a, _, b = r.partition('..')
            unified.append((int(a, 16), int(b or a, 16)))
    l = l.split('#')[0].strip()
    if not l or l.startswith('[') or ';' not in l:
        continue
    k, v = l.split(';', 1)
    ces = re.findall(r'\[([^\]]*)\]', v)
    if '|' in k:
        prefix.append((k, ces))
        continue
    ent.append(([int(x, 16) for x in k.split()], ces))

def b(s):
    return bytes(int(x, 16) for x in s.split())

def fields(c):
    return [x.strip() for x in c.split(',')]

P, S, T = set(), set(), set()
for _, ces in ent + [(None, c) for _, c in prefix]:
    for c in ces:
        f = fields(c)
        if f[0].startswith('U+'):
            S.add('05')
            T.add(f[1] if len(f) > 1 else '05')
            continue
        P.add(f[0]); S.add(f[1]); T.add(f[2])

def tmask(t):
    v = int.from_bytes(b(t).ljust(2, b'\0'), 'big')
    return v & 0x3f3f, (v >> 14) & 3

prank = {p: i for i, p in enumerate(sorted(P, key=lambda x: b(x).ljust(4, b'\0')))}
srank = {s: i for i, s in enumerate(sorted(S, key=lambda x: b(x).ljust(2, b'\0')))}
tvals = sorted({tmask(t)[0] for t in T})
trank = {v: i for i, v in enumerate(tvals)}
assert prank[''] == 0 and srank[''] == 0 and trank[0] == 0
assert len(prank) < 65536 and len(srank) < 512
assert len(trank) <= 32

def lead(p):
    return b(p)[0] if p else 0
merge_rank = max(r for p, r in prank.items() if p and lead(p) <= 0x02)
var_top = max(r for p, r in prank.items() if p and lead(p) <= 0x0B)
trail = min(r for p, r in prank.items() if p and lead(p) >= 0xE5)
assert all(lead(p) < 0xE0 or lead(p) >= 0xE5 or p == 'E4' for p in P)

def pack(c):
    f = fields(c)
    if f[0].startswith('U+'):
        cp = int(f[0][2:], 16)
        t, case = tmask(f[1] if len(f) > 1 else '05')
        return [0xFFFFFFFF, cp | trank[t] << 21 | case << 26]
    t, case = tmask(f[2])
    return [prank[f[0]] << 16 | srank[f[1]] << 7 | trank[t] << 2 | case]

pool = []
def intern(words):
    # Reuse an earlier identical run of words.
    n = len(words)
    for i in range(len(pool) - n + 1):
        if pool[i:i + n] == words:
            return i
    pool.extend(words)
    return len(pool) - n

singles = sorted((c[0], ces) for c, ces in ent if len(c) == 1)
contr = sorted(((c, ces) for c, ces in ent if len(c) > 1), key=lambda e: (e[0][0], -len(e[0]), e[0]))
MAP = []
i = 0
while i < len(singles):
    cp, ces = singles[i]
    words = [w for c in ces for w in pack(c)]
    if len(words) == 1 and words[0] != 0xFFFFFFFF:
        # A run: consecutive code points whose single CEs step the primary by 1 (or 0).
        j = i + 1
        step = None
        while j < len(singles) and j - i < 512:
            cq, cs = singles[j]
            ws = [w for c in cs for w in pack(c)]
            if cq != cp + (j - i) or len(ws) != 1:
                break
            d = ws[0] - words[0]
            want = (j - i) << 16
            if step is None and d in (0, 1 << 16):
                step = 0 if d == 0 else 1
            if not ((step == 1 and d == want) or (step == 0 and d == 0)):
                break
            j += 1
        kind = 0 if step in (None, 1) else 1
        MAP.append((cp << 11 | kind << 9 | (j - i - 1), words[0]))
        i = j
        continue
    assert len(words) < 32
    MAP.append((cp << 11 | 2 << 9, intern(words) << 5 | len(words)))
    i += 1

SEQ = []
CONTR = []
for cps, ces in contr:
    words = [w for c in ces for w in pack(c)]
    assert len(cps) < 8 and len(words) < 32
    CONTR.append((len(SEQ) << 3 | len(cps), intern(words) << 5 | len(words)))
    SEQ.extend(cps)

pre = {}
for k, ces in prefix:
    a, _, c = k.partition('|')
    pre.setdefault(int(c.strip(), 16), []).append((int(a.strip(), 16), pack(ces[0])[0]))

zeros = sorted(c for c in range(0x110000) if unicodedata.category(chr(c)) == 'Nd' and unicodedata.decimal(chr(c)) == 0)

# Han in radical-stroke order (CLDR root's implicit order): runs of code points whose
# order positions are consecutive, by start: (start - 0x3400) | position << 18, 5 bytes LE.
rad_order = []
for l in src:
    if l.startswith('[radical') and ':' in l:
        body = l[l.index(':') + 1:l.rindex(']')]
        i = 0
        while i < len(body):
            a = ord(body[i])
            if i + 2 < len(body) and body[i + 1] == '-':
                rad_order.extend(range(a, ord(body[i + 2]) + 1)); i += 3
            else:
                rad_order.append(a); i += 1
han_pos = {c: n for n, c in enumerate(rad_order)}
assert len(han_pos) == len(rad_order) == sum(b - a + 1 for a, b in unified)
han = bytearray()
prev = None
for c in sorted(han_pos):
    if prev is not None and c == prev + 1 and han_pos[c] == han_pos[prev] + 1:
        prev = c
        continue
    assert 0x3400 <= c < 0x3400 + (1 << 18) and han_pos[c] < (1 << 22)
    han += ((c - 0x3400) | han_pos[c] << 18).to_bytes(5, 'little')
    prev = c
open(sys.argv[2], 'wb').write(bytes(han))

def rows(xs, fmt, per):
    out = []
    for i in range(0, len(xs), per):
        out.append('    ' + ' '.join(fmt(x) + ',' for x in xs[i:i + per]))
    return '\n'.join(out)

print(f'''//! The CLDR root collation (UCA {version}) for Intl.Collator, generated by
//! scripts/gen-collation.py from CLDR's FractionalUCA_SHORT.txt (the data ICU's root
//! collator is built from). Do not edit by hand.
//!
//! A packed collation element (CE) is `primary rank << 16 | secondary rank << 7 |
//! tertiary rank << 2 | case (0 lower, 1 mixed, 2 upper)`, each rank dense in its level's weight order (0: none).
//! `IMPLICIT` followed by `cp | tertiary rank << 21 | case << 26` stands for the implicit CE of `cp`.
#![allow(clippy::unreadable_literal)]

pub(super) const IMPLICIT: u32 = 0xFFFF_FFFF;
/// The last variable primary rank (spaces and punctuation, ICU's default maxVariable).
pub(super) const VARIABLE_TOP: u32 = {var_top};
/// The first primary rank after the implicit (Han and unassigned) primaries.
pub(super) const TRAILING: u32 = {trail};
/// The secondary and tertiary ranks of the common weight (05).
pub(super) const COMMON_S: u16 = {srank['05']};
pub(super) const COMMON_T: u8 = {trank[0x0500]};
/// The primary rank of digit zero (numeric collation encodes numbers there).
pub(super) const DIGIT_ZERO: u32 = {pack(singles[[c for c, _ in singles].index(0x30)][1][0])[0] >> 16};
/// Unified_Ideograph ranges (Han: after the table's primaries, by radical and strokes).
pub(super) const UNIFIED: &[(u32, u32)] = &[
{rows(unified, lambda r: f'(0x{r[0]:X}, 0x{r[1]:X})', 6)}
];
/// Code points with a table entry: `start << 11 | kind << 9 | len - 1`, then for kind 0 a
/// CE whose primary steps by one per code point, kind 1 one CE for every code point, kind 2
/// `pool index << 5 | words`.
pub(super) static MAP: &[(u32, u32)] = &[
{rows(MAP, lambda e: f'(0x{e[0]:X}, 0x{e[1]:X})', 5)}
];
/// Contractions sorted by first code point, longest first: `SEQ offset << 3 | len`,
/// `pool index << 5 | words`.
pub(super) static CONTR: &[(u32, u32)] = &[
{rows(CONTR, lambda e: f'(0x{e[0]:X}, 0x{e[1]:X})', 5)}
];
pub(super) static SEQ: &[u32] = &[
{rows(SEQ, lambda x: f'0x{x:X}', 10)}
];
pub(super) static POOL: &[u32] = &[
{rows(pool, lambda x: f'0x{x:X}', 8)}
];
/// Prefix mappings: (code point, preceding code point, CE).
pub(super) const PREFIX: &[(u32, u32, u32)] = &[
{rows([(c, a, w) for c, l in sorted(pre.items()) for a, w in l], lambda e: f'(0x{e[0]:X}, 0x{e[1]:X}, 0x{e[2]:X})', 3)}
];
/// The zero of each run of ten decimal digits (Nd), for numeric collation.
pub(super) const DIGIT_ZEROS: &[u32] = &[
{rows(zeros, lambda x: f'0x{x:X}', 10)}
];''')
print(f'MAP {len(MAP)} CONTR {len(CONTR)} SEQ {len(SEQ)} POOL {len(pool)} zeros {len(zeros)} unidata {unicodedata.unidata_version}', file=sys.stderr)
