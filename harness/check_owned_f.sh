#!/bin/bash
# Phase-F owned-code audit: diff ONLY phase-F owned codes (multiset + order).
# Owned: R0901-R0917 (design), R0401 (cyclic-import), R0801 (duplicate-code),
# R17xx (refactoring), C1804/C1805 (refactoring default-off), W1113/W1116
# (typecheck-W). Uses run_full.sh output naming (.<profile>.ours).
set -u
ROOT=~/Desktop/Projects/prylint
C="$1"; P="$2"
GT=/tmp/$C.$P.fgt
python3 $ROOT/harness/strip_footer.py $ROOT/harness/results/$C.$P.out > $GT
OURS=$ROOT/harness/results/$C.$P.ours
python3 - "$GT" "$OURS" <<'PYEOF'
import re, sys
OWNED = set((
 "R0901 R0902 R0903 R0904 R0911 R0912 R0913 R0914 R0915 R0916 R0917 "
 "R0401 R0801 "
 "R1701 R1702 R1703 R1704 R1705 R1706 R1707 R1708 R1709 R1710 R1711 R1712 "
 "R1713 R1714 R1715 R1716 R1717 R1718 R1719 R1720 R1721 R1722 R1723 R1724 "
 "R1725 R1726 R1727 R1728 R1729 R1730 R1731 R1732 R1733 R1734 R1735 R1736 R1737 "
 "C1804 C1805 W1113 W1116").split())
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
N=int(__import__('os').environ.get('SHOW','8'))
for k in list(fn)[:N]: print("   -", k)
for k in list(fp)[:N]: print("   +", k)
if not fn and not fp:
    print("  order:", "EXACT" if gt == ours else "MISMATCH")
    if gt != ours:
        for i, (a, b) in enumerate(zip(gt, ours)):
            if a != b:
                print(f"   first divergence at #{i}:\n    GT  {a}\n    OUR {b}")
                break
PYEOF
