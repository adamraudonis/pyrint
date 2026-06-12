#!/bin/bash
# Phase-B zero-round helper: for corpus+profile, diff ONLY the owned codes
# (multiset via diffmsg --, plus exact owned-subsequence order check).
set -u
ROOT=~/Desktop/Projects/prylint
C="$1"; P="$2"
GT=/tmp/$C.$P.gt
python3 $ROOT/harness/strip_footer.py $ROOT/harness/results/$C.$P.out > $GT
OURS=$ROOT/harness/results/$C.ours$P.out
python3 - "$GT" "$OURS" <<'EOF'
import re, sys
OWNED = set("W0101 W0102 W0104 W0105 W0106 W0107 W0108 W0109 W0120 W0122 W0123 W0125 W0126 W0127 W0128 W0129 W0130 W0131 W0133 W0134 W0150 W0199 C0121 C0123 R0123 R0124 R0133 W0143 W0177 C0103 C0104 C0105 C0131 C0132 C0112 C0114 C0115 C0116 W0135 W0136 W0137".split())
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
for k in list(fn)[:8]: print("   -", k)
for k in list(fp)[:8]: print("   +", k)
if not fn and not fp:
    print("  order:", "EXACT" if gt == ours else "MISMATCH")
    if gt != ours:
        for i, (a, b) in enumerate(zip(gt, ours)):
            if a != b:
                print(f"   first divergence at #{i}:\n    GT  {a}\n    OUR {b}")
                break
EOF
