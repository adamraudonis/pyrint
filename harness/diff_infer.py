#!/usr/bin/env python3
"""Show per-file line diffs between GT infercache and the last rust dump.

Usage: diff_infer.py <corpus> [file_substring] [--max=N]
Reads /tmp/inferdump_rs_<corpus>.out and harness/infercache/<corpus>/.
Prints, for each differing file, paired GT/OURS lines (positionally aligned).
"""

import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def parse_rs(corpus):
    rs, cur, buf = {}, None, []
    for line in open(f"/tmp/inferdump_rs_{corpus}.out", encoding="utf-8", errors="replace"):
        if line.startswith("=== "):
            if cur is not None:
                rs[cur] = "".join(buf)
            cur, buf = line[4:].strip(), []
        elif cur is not None:
            buf.append(line)
    if cur is not None:
        rs[cur] = "".join(buf)
    return rs


def main():
    corpus = sys.argv[1]
    sub = None
    maxn = 10**9
    for a in sys.argv[2:]:
        if a.startswith("--max="):
            maxn = int(a.split("=")[1])
        else:
            sub = a
    cache = os.path.join(ROOT, "harness", "infercache", corpus)
    rs = parse_rs(corpus)
    shown = 0
    for f in open(f"/tmp/inferdiffs_{corpus}.txt"):
        f = f.strip().split(" ", 1)[1]
        if sub and sub not in f:
            continue
        p = os.path.join(cache, f.lstrip("./") + ".dump")
        gt = open(p, encoding="utf-8", errors="replace").read().splitlines()
        ours = rs.get(f, "").splitlines()
        print(f"##### {f} (gt {len(gt)} lines, ours {len(ours)})")
        import difflib
        sm = difflib.SequenceMatcher(None, gt, ours, autojunk=False)
        for tag, i1, i2, j1, j2 in sm.get_opcodes():
            if tag == "equal":
                continue
            for k in range(max(i2 - i1, j2 - j1)):
                g = gt[i1 + k] if i1 + k < i2 else "<absent>"
                o = ours[j1 + k] if j1 + k < j2 else "<absent>"
                print(f"  GT  {g}")
                print(f"  RS  {o}")
                shown += 1
                if shown >= maxn:
                    return
    return


if __name__ == "__main__":
    main()
