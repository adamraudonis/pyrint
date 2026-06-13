#!/usr/bin/env python3
"""Phase-F round-10 owned-code audit across all 27 corpora x 2 profiles.

For each (corpus, profile):
  - footer-strip the GT, parse owned-code message lines
  - parse ours owned-code message lines
  - report FP/FN per code, and whether order is EXACT when zero diff
For the OOM'd core.full GT (and any SUSPECT GT), additionally print a
restricted-to-GT-reached-files comparison (the authoritative measure).

Aggregates a global per-code FP/FN total across all clean combos.
"""
import os, re, subprocess, sys
from collections import Counter

ROOT = os.path.expanduser("~/Desktop/Projects/prylint")
RES = os.path.join(ROOT, "harness", "results")
CORPORA = ("scrapy rich werkzeug tornado celery fastapi pip botocore mypy pydantic "
           "matplotlib ansible twisted sqlalchemy zulip django salt scikit-learn nova "
           "numpy sentry pandas sympy airflow pylfunc core black").split()

OWNED = set((
 "R0901 R0902 R0903 R0904 R0911 R0912 R0913 R0914 R0915 R0916 R0917 "
 "R0401 R0801 "
 "R1701 R1702 R1703 R1704 R1705 R1706 R1707 R1708 R1709 R1710 R1711 R1712 "
 "R1713 R1714 R1715 R1716 R1717 R1718 R1719 R1720 R1721 R1722 R1723 R1724 "
 "R1725 R1726 R1727 R1728 R1729 R1730 R1731 R1732 R1733 R1734 R1735 R1736 R1737 "
 "C1804 C1805 W1113 W1116").split())
CLOSE_CODES = {"R0801", "R0401"}
MSGLINE = re.compile(r"^([^:]+):(\d+):(\d+): ([A-Z]\d{4}):")

def strip_footer(path):
    return subprocess.run(["python3", os.path.join(ROOT, "harness", "strip_footer.py"), path],
                          capture_output=True, text=True).stdout

def owned_lines(text):
    out = []
    for line in text.split("\n"):
        m = MSGLINE.match(line)
        if m and m.group(4) in OWNED:
            out.append(line)
    return out

def files_with_msgs(text):
    fs = set()
    for line in text.split("\n"):
        m = MSGLINE.match(line)
        if m:
            fs.add(m.group(1))
    return fs

def by_code(c):
    r = Counter()
    for k, v in c.items():
        r[MSGLINE.match(k).group(4)] += v
    return dict(sorted(r.items()))

# detect SUSPECT GTs
suspect = {}
for c in CORPORA:
    for p in ("hook", "full"):
        ex = os.path.join(RES, f"{c}.{p}.exit")
        out = os.path.join(RES, f"{c}.{p}.out")
        if not os.path.exists(out):
            continue
        exitcode = open(ex).read().strip() if os.path.exists(ex) else "?"
        data = open(out, "rb").read()
        if exitcode in ("143", "137"):
            suspect[(c, p)] = f"exit={exitcode}(killed)"
        elif not data.endswith(b"\n"):
            text = data.decode("utf-8", "replace")
            last = next((ln for ln in reversed(text.split("\n")) if ln.strip()), "")
            if re.match(r"^\*{13} Module ", last):
                suspect[(c, p)] = "bare-module-header-no-nl"

glob_fp = Counter(); glob_fn = Counter()
nonzero = []
allcombos = 0
zerocombos = 0
for c in CORPORA:
    for p in ("hook", "full"):
        out = os.path.join(RES, f"{c}.{p}.out")
        ours_p = os.path.join(RES, f"{c}.{p}.ours")
        if not os.path.exists(out) or not os.path.exists(ours_p):
            print(f"[{c}.{p}] MISSING capture")
            continue
        allcombos += 1
        gt_text = strip_footer(out)
        ours_text = open(ours_p, encoding="utf-8", errors="replace").read()
        gt = owned_lines(gt_text)
        ours = owned_lines(ours_text)
        cg, co = Counter(gt), Counter(ours)
        fn = cg - co; fp = co - cg
        tag = ""
        if (c, p) in suspect:
            tag = f"  [SUSPECT-GT: {suspect[(c,p)]}]"
        nf, nfp = sum(fn.values()), sum(fp.values())
        if nf == 0 and nfp == 0:
            order = "EXACT" if gt == ours else "ORDER-MISMATCH"
            print(f"[{c}.{p}] 0FP/0FN GT={len(gt)} ours={len(ours)} order={order}{tag}")
            if order == "EXACT":
                zerocombos += 1
            else:
                nonzero.append((c, p, "ORDER"))
                for i,(a,b) in enumerate(zip(gt,ours)):
                    if a!=b:
                        print(f"    first divergence #{i}:\n      GT  {a}\n      OUR {b}")
                        break
        else:
            print(f"[{c}.{p}] FP={nfp} FN={nf} GT={len(gt)} ours={len(ours)}{tag}")
            if fp: print("    FP:", by_code(fp))
            if fn: print("    FN:", by_code(fn))
            # restricted comparison for suspect GTs
            if (c, p) in suspect:
                gtf = files_with_msgs(gt_text)
                rest_ours = [l for l in ours if MSGLINE.match(l).group(1) in gtf]
                # exclude close codes (killed run never wrote them)
                gt2 = [l for l in gt if MSGLINE.match(l).group(4) not in CLOSE_CODES]
                ro2 = [l for l in rest_ours if MSGLINE.match(l).group(4) not in CLOSE_CODES]
                cg2, co2 = Counter(gt2), Counter(ro2)
                fn2 = cg2 - co2; fp2 = co2 - cg2
                rn, rfp = sum(fn2.values()), sum(fp2.values())
                rorder = "EXACT" if gt2 == ro2 else "MISMATCH"
                print(f"    RESTRICTED (GT-reached={len(gtf)}, excl-close): "
                      f"FP={rfp} FN={rn} GT={len(gt2)} ours={len(ro2)} order={rorder}")
                if fp2: print("      rFP:", by_code(fp2))
                if fn2: print("      rFN:", by_code(fn2))
                if rn==0 and rfp==0 and rorder=="EXACT":
                    pass  # restricted-clean; not counted toward glob (suspect)
                else:
                    nonzero.append((c, p, "RESTRICTED-NONZERO"))
            else:
                nonzero.append((c, p, "NONZERO"))
                glob_fp += fp; glob_fn += fn

print("\n==== SUMMARY ====")
print(f"combos audited: {allcombos}; clean-EXACT: {zerocombos}")
print(f"non-clean combos: {[(c,p,t) for c,p,t in nonzero]}")
print(f"global owned FP by code: {dict(sorted(glob_fp.items()))}")
print(f"global owned FN by code: {dict(sorted(glob_fn.items()))}")
