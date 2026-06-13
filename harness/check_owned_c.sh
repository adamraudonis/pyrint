#!/bin/bash
# Phase-C audit: diff ONLY the phase-C owned codes (multiset + exact order).
set -u
ROOT=~/Desktop/Projects/prylint
C="$1"; P="$2"
GT=/tmp/$C.$P.gt
python3 $ROOT/harness/strip_footer.py $ROOT/harness/results/$C.$P.out > $GT
OURS=$ROOT/harness/results/$C.ours$P.out
python3 - "$GT" "$OURS" <<'PYEOF'
import re, sys
OWNED = set(("W0601 W0602 W0603 W0604 W0611 W0612 W0613 W0614 W0621 W0622 W0631 W0632 W0640 W0641 W0642 W0644 "
"C0410 C0411 C0412 C0413 C0414 C0415 W0401 W0404 W0406 W0410 W0416 R0401 R0402 "
"W0201 W0211 W0212 W0221 W0222 W0223 W0231 W0233 W0236 W0237 W0239 W0240 W0245 W0246 R0202 R0203 R0205 R0206 C0202 C0203 C0204 C0205 "
"W0702 W0705 W0706 W0711 W0716 W0718 W0719").split())
def owned_lines(p):
    out = []
    for line in open(p, encoding="utf-8", errors="replace"):
        m = re.match(r"[^:]+:\d+:\d+: ([A-Z]\d{4}):", line)
        if m and m.group(1) in OWNED:
            out.append(line.rstrip("\n"))
    return out
gt, ours = owned_lines(sys.argv[1]), owned_lines(sys.argv[2])
from collections import Counter
cg, co = Counter(gt), Counter(ours)
fn = cg - co; fp = co - cg
def by_code(c):
    r = Counter()
    for k, v in c.items():
        r[re.search(r": ([A-Z]\d{4}):", k).group(1)] += v
    return dict(sorted(r.items()))
print(f"owned GT={len(gt)} ours={len(ours)} FN={sum(fn.values())} FP={sum(fp.values())}")
if fn: print("  FN:", by_code(fn))
if fp: print("  FP:", by_code(fp))
for k in list(fn)[:6]: print("   -", k)
for k in list(fp)[:6]: print("   +", k)
if not fn and not fp:
    print("  order:", "EXACT" if gt == ours else "MISMATCH")
    if gt != ours:
        for i, (a, b) in enumerate(zip(gt, ours)):
            if a != b:
                print(f"   first divergence at #{i}:\n    GT  {a}\n    OUR {b}")
                break
PYEOF
