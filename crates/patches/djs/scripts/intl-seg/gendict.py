#!/usr/bin/env python3
"""Builds Intl.Segmenter's word dictionaries (crates/djs-core/src/builtins/intl/segdict/*.dict)
from ICU 78.3's break-iterator dictionary sources, the data node 22.23.5 (ICU 78.3) builds
its cjdict, thaidict, laodict, burmesedict and khmerdict tries from:

    scripts/intl-seg/fetch.sh DIR && scripts/intl-seg/gendict.py DIR OUTDIR

Format (little endian), one file per dictionary: u32 word count, u32 block count, u16 rank
count, u8 has_values, u8 block size; the ranked code points (u16 each, most frequent
first); the byte offset of each block (u32); then the words, sorted by code point, front
coded: a header byte (shared prefix << 4 | suffix length - 1, or 0xF0 then the prefix and
suffix lengths as bytes), the suffix's code points as ranks (one byte below 128, else two:
0x80 | rank >> 8, rank & 0xff), then the cost byte when has_values. Each block's first word
shares nothing, so a block decodes on its own.
"""
import os, sys, struct, collections

R = 32
NAMES = [("cjdict", "cj"), ("thaidict", "th"), ("laodict", "lo"), ("burmesedict", "my"), ("khmerdict", "km")]


def load(path):
    words = {}
    for line in open(path, encoding="utf-8-sig"):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split()
        if parts[0] in words:
            sys.stderr.write("duplicate %r in %s\n" % (parts[0], path))
        words[parts[0]] = int(parts[1]) if len(parts) > 1 else None
    return words


def build(words):
    ws = sorted(words)
    vals = any(words[w] is not None for w in ws)
    cnt = collections.Counter(ch for w in ws for ch in w)
    ranked = [c for c, _ in sorted(cnt.items(), key=lambda kv: (-kv[1], kv[0]))]
    assert all(ord(c) < 0x10000 for c in ranked) and len(ranked) < 0x8000
    rank = {c: i for i, c in enumerate(ranked)}
    out, offsets, prev = bytearray(), [], ""
    for i, w in enumerate(ws):
        k = 0
        if i % R == 0:
            offsets.append(len(out))
        else:
            while k < len(w) and k < len(prev) and w[k] == prev[k]:
                k += 1
        n = len(w) - k
        assert n >= 1
        if k < 15 and n <= 16:
            out.append(k << 4 | (n - 1))
        else:
            out += bytes([0xF0, k, n])
        for ch in w[k:]:
            r = rank[ch]
            out += bytes([r]) if r < 128 else bytes([0x80 | r >> 8, r & 0xFF])
        if vals:
            v = words[w]
            assert v is not None and 0 <= v < 256
            out.append(v)
        prev = w
    head = struct.pack("<IIHBB", len(ws), len(offsets), len(ranked), int(vals), R)
    head += b"".join(struct.pack("<H", ord(c)) for c in ranked)
    head += b"".join(struct.pack("<I", o) for o in offsets)
    return head + bytes(out)


def main():
    src, dst = sys.argv[1], sys.argv[2]
    os.makedirs(dst, exist_ok=True)
    total = 0
    for name, short in NAMES:
        data = build(load(os.path.join(src, name + ".txt")))
        open(os.path.join(dst, short + ".dict"), "wb").write(data)
        total += len(data)
        print("%s.dict %d bytes" % (short, len(data)))
    print("total %d bytes" % total)


if __name__ == "__main__":
    main()

