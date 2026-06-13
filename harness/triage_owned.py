#!/usr/bin/env python3
"""Triage owned phase-E codes across all corpora and both profiles.

For each (corpus, profile): footer-strip GT, parse owned-code messages from
GT and ours, report FP (we emit, GT doesn't) and FN (GT has, we miss) per
owned code. Owned codes are passed via OWNED env or default below.
"""
import os
import re
import sys
from collections import Counter

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from strip_footer import strip_footer  # noqa

LINE = re.compile(r"^(?P<path>[^:]+):(?P<line>\d+):(?P<col>\d+): (?P<code>[A-Z]\d{4}): (?P<msg>.*) \((?P<sym>[a-z0-9-]+)\)$")

OWNED = set((os.environ.get("OWNED") or
             "R0901,R0902,R0903,R0904,R0911,R0912,R0913,R0914,R0915,R0916,R0917,R0801").split(","))

CORPORA = ["scrapy", "rich", "werkzeug", "tornado", "celery", "fastapi", "black",
           "pip", "botocore", "mypy", "pydantic", "matplotlib", "ansible", "twisted",
           "sqlalchemy", "zulip", "django", "salt", "scikit-learn", "nova", "numpy",
           "sentry", "pandas", "sympy", "airflow", "pylfunc", "core"]

RESULTS = os.path.join(os.path.dirname(os.path.abspath(__file__)), "results")


def parse_owned(data: bytes):
    msgs = []
    for raw in data.decode("utf-8", "replace").split("\n"):
        m = LINE.match(raw)
        if m and m["code"] in OWNED:
            msgs.append((m["path"], int(m["line"]), int(m["col"]), m["code"], m["msg"]))
    return msgs


def main():
    grand_fp = Counter()
    grand_fn = Counter()
    only = sys.argv[1] if len(sys.argv) > 1 else None
    profiles = [sys.argv[2]] if len(sys.argv) > 2 else ["hook", "full"]
    for c in (CORPORA if not only else [only]):
        for p in profiles:
            gtp = os.path.join(RESULTS, f"{c}.{p}.out")
            ourp = os.path.join(RESULTS, f"{c}.{p}.ours")
            if not os.path.exists(gtp) or not os.path.exists(ourp):
                print(f"{c}.{p}: MISSING ({os.path.exists(gtp)},{os.path.exists(ourp)})")
                continue
            gt = parse_owned(strip_footer(open(gtp, "rb").read()))
            our = parse_owned(open(ourp, "rb").read())
            gtc = Counter(gt)
            ourc = Counter(our)
            fp = ourc - gtc
            fn = gtc - ourc
            if fp or fn:
                fpc = Counter()
                fnc = Counter()
                for m, n in fp.items():
                    fpc[m[3]] += n
                for m, n in fn.items():
                    fnc[m[3]] += n
                parts = []
                for code in sorted(set(fpc) | set(fnc)):
                    parts.append(f"{code}:{fpc[code]}FP/{fnc[code]}FN")
                    grand_fp[code] += fpc[code]
                    grand_fn[code] += fnc[code]
                print(f"{c}.{p}: " + "  ".join(parts))
    print("=== GRAND TOTALS ===")
    for code in sorted(OWNED):
        if grand_fp[code] or grand_fn[code]:
            print(f"  {code}: {grand_fp[code]}FP/{grand_fn[code]}FN")
    total = sum(grand_fp.values()) + sum(grand_fn.values())
    print(f"TOTAL: {sum(grand_fp.values())}FP/{sum(grand_fn.values())}FN  (sum={total})")


if __name__ == "__main__":
    main()
