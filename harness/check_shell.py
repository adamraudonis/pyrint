#!/usr/bin/env python3
"""Acceptance gate for the lint-pipeline shell (and later, full parity).

Usage: check_shell.py <corpus> <ours.out> <ours.exit> [--owned=E0001,E0011,E25]

Asserts, against harness/results/<corpus>.iso.out:
 1. ZERO false positives (every line we emit, pylint emits).
 2. Our message lines form an ordered SUBSEQUENCE of pylint's (headers incl.),
    validating emission order without requiring all checkers.
 3. For OWNED codes (prefix match), zero false negatives — except E0001
    'Cannot import' lines (need module graph; explicitly deferred).
 4. Exit code self-consistency: equals bitmask of OUR displayed messages
    (F=1,E=2,W=4,R=8,C=16), or full equality if --strict-exit.
Exit 0 = pass.
"""

import os
import re
import sys

MSG = re.compile(r"^[^:]+:\d+:\d+: ([A-Z]\d{4}): ")
BITS = {"F": 1, "E": 2, "W": 4, "R": 8, "C": 16, "I": 0}


def main():
    corpus, ours_path, ours_exit = sys.argv[1], sys.argv[2], sys.argv[3]
    owned = ["E0001", "E0011", "E25"]
    strict_exit = False
    for a in sys.argv[4:]:
        if a.startswith("--owned="):
            owned = a.split("=", 1)[1].split(",")
        if a == "--strict-exit":
            strict_exit = True
    gt_path = os.path.expanduser(
        f"~/Desktop/Projects/prylint/harness/results/{corpus}.iso.out"
    )
    gt = open(gt_path, encoding="utf-8").read().splitlines()
    ours = open(ours_path, encoding="utf-8").read().splitlines()

    fails = []

    # 1+2: subsequence check
    it = iter(enumerate(gt))
    missing_from_gt = []
    for line in ours:
        for _, g in it:
            if g == line:
                break
        else:
            missing_from_gt.append(line)
            it = iter(enumerate(gt))  # reset not needed; FP regardless
            break
    if missing_from_gt:
        # precise FP listing (set-based) for diagnostics
        gtset = {}
        for g in gt:
            gtset[g] = gtset.get(g, 0) + 1
        fps = []
        for line in ours:
            if gtset.get(line, 0) > 0:
                gtset[line] -= 1
            elif MSG.match(line) or line.startswith("*"):
                fps.append(line)
        if fps:
            fails.append(f"FALSE POSITIVES ({len(fps)}):")
            fails.extend("  " + f for f in fps[:10])
        else:
            fails.append("ORDERING: our lines are not a subsequence of ground truth")
            fails.append(f"  first out-of-order: {missing_from_gt[0]}")

    # 3: owned-code FNs
    ours_set = {}
    for line in ours:
        ours_set[line] = ours_set.get(line, 0) + 1
    fn = []
    for g in gt:
        m = MSG.match(g)
        if not m:
            continue
        code = m.group(1)
        if not any(code.startswith(o) for o in owned):
            continue
        if "E0001: Cannot import '" in g:
            continue  # deferred: needs module graph
        if ours_set.get(g, 0) > 0:
            ours_set[g] -= 1
        else:
            fn.append(g)
    if fn:
        fails.append(f"OWNED-CODE FALSE NEGATIVES ({len(fn)}):")
        fails.extend("  " + f for f in fn[:10])

    # 4: exit code
    bits = 0
    for line in ours:
        m = MSG.match(line)
        if m:
            bits |= BITS[m.group(1)[0]]
    actual = int(open(ours_exit).read().strip()) if os.path.exists(ours_exit) else int(ours_exit)
    if strict_exit:
        gt_exit = int(open(gt_path.replace(".out", ".exit")).read().strip())
        if actual != gt_exit:
            fails.append(f"EXIT: ours {actual} != pylint {gt_exit}")
    elif actual != bits:
        fails.append(f"EXIT SELF-CONSISTENCY: ours {actual} != computed {bits}")

    if fails:
        print(f"{corpus}: FAIL")
        print("\n".join(fails))
        return 1
    print(f"{corpus}: PASS (ours {sum(1 for l in ours if MSG.match(l))} msgs, "
          f"gt {sum(1 for l in gt if MSG.match(l))})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
