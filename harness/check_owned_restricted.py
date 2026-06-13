#!/usr/bin/env python3
"""Owned-code diff restricted to the files the (possibly truncated) GT reached.

For truncated GT captures (sentry.hook mid-stream cut, core.full SIGKILL before
close()), the naive diff shows huge FP counts for owned-code lines in files the
GT never streamed. This restricts BOTH sides to:
  (1) only files for which the GT emitted a '************* Module X' header AND
      at least one message line (so a bare-header-at-EOF file is excluded), and
  (2) excludes close()-time codes (R0801, R0401) that a killed run never wrote.

Usage: check_owned_restricted.py <corpus> <profile> [--exclude-close]
"""
import os, re, sys
from collections import Counter

ROOT = os.path.expanduser("~/Desktop/Projects/prylint")
C, P = sys.argv[1], sys.argv[2]
EXCLUDE_CLOSE = "--exclude-close" in sys.argv[3:]

OWNED = set((
 "R0901 R0902 R0903 R0904 R0911 R0912 R0913 R0914 R0915 R0916 R0917 "
 "R0401 R0801 "
 "R1701 R1702 R1703 R1704 R1705 R1706 R1707 R1708 R1709 R1710 R1711 R1712 "
 "R1713 R1714 R1715 R1716 R1717 R1718 R1719 R1720 R1721 R1722 R1723 R1724 "
 "R1725 R1726 R1727 R1728 R1729 R1730 R1731 R1732 R1733 R1734 R1735 R1736 R1737 "
 "C1804 C1805 W1113 W1116").split())
CLOSE_CODES = {"R0801", "R0401"}

# build footer-stripped GT first
gt_raw = os.path.join(ROOT, "harness", "results", f"{C}.{P}.out")
import subprocess
gt = subprocess.run(["python3", os.path.join(ROOT, "harness", "strip_footer.py"), gt_raw],
                    capture_output=True, text=True).stdout
ours = open(os.path.join(ROOT, "harness", "results", f"{C}.{P}.ours"),
            encoding="utf-8", errors="replace").read()

HDR = re.compile(r"^\*{13} Module (.+)$")
MSGLINE = re.compile(r"^([^:]+):(\d+):(\d+): ([A-Z]\d{4}):")

def parse(text):
    """Return (files_with_msgs:set(path), owned_lines:list, per_file_owned:dict)."""
    files = set()
    owned = []
    for line in text.split("\n"):
        m = MSGLINE.match(line)
        if not m:
            continue
        path, code = m.group(1), m.group(4)
        files.add(path)
        if code in OWNED:
            if EXCLUDE_CLOSE and code in CLOSE_CODES:
                continue
            owned.append(line)
    return files, owned

gt_files, gt_owned = parse(gt)
ours_files, ours_owned = parse(ours)

# Restrict ours owned-lines to files the GT actually reached.
restricted_ours = [l for l in ours_owned if MSGLINE.match(l).group(1) in gt_files]

cg, co = Counter(gt_owned), Counter(restricted_ours)
fn = cg - co
fp = co - cg
def by_code(c):
    r = Counter()
    for k, v in c.items():
        r[MSGLINE.match(k).group(4)] += v
    return dict(sorted(r.items()))
print(f"[{C}.{P}] restricted to {len(gt_files)} GT-reached files; "
      f"exclude_close={EXCLUDE_CLOSE}")
print(f"  owned GT={len(gt_owned)} ours_restricted={len(restricted_ours)} "
      f"FN={sum(fn.values())} FP={sum(fp.values())}")
if fn: print("  FN:", by_code(fn))
if fp: print("  FP:", by_code(fp))
if not fn and not fp:
    print("  order:", "EXACT" if gt_owned == restricted_ours else "MISMATCH")
