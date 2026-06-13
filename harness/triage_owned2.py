#!/usr/bin/env python3
"""Block-aware owned-code comparison (handles R0801 multi-line blocks).

Parses pylint text output into a stream of (kind, payload) where each
message owns the lines until the next module-header / next message line /
EOF. R0801 messages carry their full duplicate-code block. Compares the
owned-code message blocks between footer-stripped GT and ours, order-aware.

Usage: triage_owned2.py <corpus> <profile>
"""
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from strip_footer import strip_footer  # noqa

RESULTS = os.path.join(os.path.dirname(os.path.abspath(__file__)), "results")
# A "message start" line: path:line:col: CODE: ...
MSG = re.compile(r"^([^:]+):(\d+):(\d+): ([A-Z]\d{4}): ")
HDR = re.compile(r"^\*\*\*\*\*\*\*\*\*\*\*\*\* ")
OWNED = set("R0901,R0902,R0903,R0904,R0911,R0912,R0913,R0914,R0915,R0916,R0917,R0801".split(","))


def blocks(data: bytes):
    """Yield (code, key, fulltext) for each message; R0801 includes its block."""
    lines = data.decode("utf-8", "replace").split("\n")
    out = []
    i = 0
    n = len(lines)
    while i < n:
        ln = lines[i]
        m = MSG.match(ln)
        if not m:
            i += 1
            continue
        code = m.group(4)
        start = i
        i += 1
        # absorb continuation lines (not a header, not a new message start)
        while i < n and not MSG.match(lines[i]) and not HDR.match(lines[i]):
            i += 1
        full = "\n".join(lines[start:i])
        key = (m.group(1), int(m.group(2)), int(m.group(3)), code)
        out.append((code, key, full))
    return out


def main():
    c, p = sys.argv[1], sys.argv[2]
    gt = blocks(strip_footer(open(os.path.join(RESULTS, f"{c}.{p}.out"), "rb").read()))
    our = blocks(open(os.path.join(RESULTS, f"{c}.{p}.ours"), "rb").read())
    gto = [b for b in gt if b[0] in OWNED]
    ouro = [b for b in our if b[0] in OWNED]
    # order-aware exact compare of full block text
    gseq = [b[2] for b in gto]
    oseq = [b[2] for b in ouro]
    if gseq == oseq:
        print(f"{c}.{p}: {len(gseq)} owned blocks (incl R0801 full text) EXACT-ORDER")
        return
    # report first divergence and multiset diff
    from collections import Counter
    gc, oc = Counter(gseq), Counter(oseq)
    fp = oc - gc
    fn = gc - oc
    print(f"{c}.{p}: GT={len(gseq)} OUR={len(oseq)}  FPblocks={sum(fp.values())} FNblocks={sum(fn.values())}")
    for i, (a, b) in enumerate(zip(gseq, oseq)):
        if a != b:
            print(f"  first order-diff @ idx {i}:")
            print("  GT :", a[:200].replace("\n", "\\n"))
            print("  OUR:", b[:200].replace("\n", "\\n"))
            break
    # per-code FP/FN
    fpc, fnc = Counter(), Counter()
    keybycode = {b[2]: b[0] for b in gto + ouro}
    for t, k in fp.items():
        fpc[keybycode.get(t, "?")] += k
    for t, k in fn.items():
        fnc[keybycode.get(t, "?")] += k
    for code in sorted(set(fpc) | set(fnc)):
        print(f"  {code}: {fpc[code]}FP/{fnc[code]}FN")


if __name__ == "__main__":
    main()
