#!/usr/bin/env python3
"""Diff two pylint text outputs message-wise and report FP/FN per code.

Usage: diffmsg.py <ground_truth.out> <candidate.out> [--samples N] [--code EXXXX]

Exit 0 if byte-identical, 1 otherwise.
"""

import re
import sys
from collections import Counter, defaultdict

LINE = re.compile(r"^(?P<path>[^:]+):(?P<line>\d+):(?P<col>\d+): (?P<code>[A-Z]\d{4}): (?P<msg>.*) \((?P<sym>[a-z0-9-]+)\)$")


def parse(path):
    msgs = []
    headers = []
    other = []
    with open(path, encoding="utf-8") as f:
        for raw in f:
            line = raw.rstrip("\n")
            if line.startswith("************* "):
                headers.append(line)
                continue
            m = LINE.match(line)
            if m:
                msgs.append((m["path"], int(m["line"]), int(m["col"]), m["code"], m["msg"], m["sym"]))
            elif line.strip():
                other.append(line)
    return msgs, headers, other


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    samples = 5
    code_filter = None
    for a in sys.argv[1:]:
        if a.startswith("--samples"):
            samples = int(a.split("=")[1])
        if a.startswith("--code"):
            code_filter = a.split("=")[1]
    gt_path, cand_path = args
    gt_raw = open(gt_path, "rb").read()
    cand_raw = open(cand_path, "rb").read()
    if gt_raw == cand_raw:
        print("BYTE-IDENTICAL")
        return 0

    gt, gt_h, gt_other = parse(gt_path)
    cand, cand_h, cand_other = parse(cand_path)

    gt_set = Counter(gt)
    cand_set = Counter(cand)
    fn = gt_set - cand_set   # in pylint, missing from us
    fp = cand_set - gt_set   # we emit, pylint doesn't

    fn_by_code = Counter()
    fp_by_code = Counter()
    for m, c in fn.items():
        fn_by_code[m[3]] += c
    for m, c in fp.items():
        fp_by_code[m[3]] += c

    print(f"ground truth: {len(gt)} msgs | candidate: {len(cand)} msgs")
    print(f"FALSE NEGATIVES (pylint has, we miss): {sum(fn.values())}")
    for code, n in fn_by_code.most_common():
        print(f"  {code}: {n}")
    print(f"FALSE POSITIVES (we emit, pylint doesn't): {sum(fp.values())}")
    for code, n in fp_by_code.most_common():
        print(f"  {code}: {n}")

    def show(title, d):
        shown = defaultdict(int)
        print(f"\n--- {title} samples ---")
        for m in sorted(d):
            if code_filter and m[3] != code_filter:
                continue
            if shown[m[3]] >= samples:
                continue
            shown[m[3]] += 1
            print(f"  {m[0]}:{m[1]}:{m[2]}: {m[3]}: {m[4]} ({m[5]})")

    show("FN", fn)
    show("FP", fp)

    if not fn and not fp:
        # same message multiset; ordering or header/other-line diffs
        print("\nMessages identical; diff is ordering/headers/other lines.")
        if gt_other or cand_other:
            print("other lines gt:", gt_other[:5], "cand:", cand_other[:5])
        # find first ordering difference
        for i, (a, b) in enumerate(zip(gt, cand)):
            if a != b:
                print(f"first order diff at msg #{i}:\n  gt:   {a}\n  cand: {b}")
                break
        if gt_h != cand_h:
            print(f"headers differ: gt {len(gt_h)} cand {len(cand_h)}")
            for a, b in zip(gt_h, cand_h):
                if a != b:
                    print(f"  gt:   {a}\n  cand: {b}")
                    break
    return 1


if __name__ == "__main__":
    sys.exit(main())
