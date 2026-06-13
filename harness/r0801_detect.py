#!/usr/bin/env python3
"""R0801 detection vs display analysis.

For each R0801 block, the "detection signature" is:
  (location-line, count, sorted ==headers)  -- everything EXCEPT the code body.
The "display body" is the rstripped real-lines code block (which couple's
region pylint chose to print -- id()-based nondeterministic per notes B.7).

Compares GT vs ours order-aware on the detection signature, and separately
reports how many blocks differ ONLY in display body (the documented artifact).

Usage: r0801_detect.py <corpus> <profile>
"""
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from strip_footer import strip_footer  # noqa

RESULTS = os.path.join(os.path.dirname(os.path.abspath(__file__)), "results")
MSG = re.compile(r"^([^:]+):(\d+):(\d+): ([A-Z]\d{4}): ")
HDR = re.compile(r"^\*\*\*\*\*\*\*\*\*\*\*\*\* ")
HEADERLINE = re.compile(r"^==[^:]+:\[\d+:\d+\]$")


def r0801_blocks(data: bytes):
    lines = data.decode("utf-8", "replace").split("\n")
    out = []
    i = 0
    n = len(lines)
    while i < n:
        m = MSG.match(lines[i])
        if not m or m.group(4) != "R0801":
            i += 1
            continue
        loc = (m.group(1), int(m.group(2)), int(m.group(3)))
        first = lines[i]
        # "Similar lines in N files"
        cnt = None
        mc = re.search(r"Similar lines in (\d+) files", first)
        if mc:
            cnt = int(mc.group(1))
        start = i
        i += 1
        body = []
        while i < n and not MSG.match(lines[i]) and not HDR.match(lines[i]):
            body.append(lines[i])
            i += 1
        headers = tuple(sorted(b for b in body if HEADERLINE.match(b)))
        code = [b for b in body if not HEADERLINE.match(b)]
        out.append({
            "loc": loc, "cnt": cnt, "headers": headers,
            "code": "\n".join(code), "full": "\n".join(lines[start:i]),
        })
    return out


def main():
    c, p = sys.argv[1], sys.argv[2]
    gt = r0801_blocks(strip_footer(open(os.path.join(RESULTS, f"{c}.{p}.out"), "rb").read()))
    our = r0801_blocks(open(os.path.join(RESULTS, f"{c}.{p}.ours"), "rb").read())
    # detection signature: order-aware
    gsig = [(b["loc"], b["cnt"], b["headers"]) for b in gt]
    osig = [(b["loc"], b["cnt"], b["headers"]) for b in our]
    det_exact = gsig == osig
    print(f"{c}.{p}: GT={len(gt)} OUR={len(our)} detection-order={'EXACT' if det_exact else 'MISMATCH'}")
    if not det_exact:
        from collections import Counter
        gc, oc = Counter(gsig), Counter(osig)
        fp = oc - gc
        fn = gc - oc
        print(f"  DETECTION FP={sum(fp.values())} FN={sum(fn.values())}")
        for i, (a, b) in enumerate(zip(gsig, osig)):
            if a != b:
                print(f"  first detection-diff @ idx {i}:\n    GT  {a}\n    OUR {b}")
                break
        return
    # detection identical & same order -> only body can differ
    body_diff = sum(1 for g, o in zip(gt, our) if g["code"] != o["code"])
    print(f"  detection IDENTICAL; display-body diffs (documented id() artifact) = {body_diff}/{len(gt)}")


if __name__ == "__main__":
    main()
