#!/usr/bin/env python3
"""Detect truncated / killed ground-truth captures.

A GT file is suspect if any of:
  * exit code is 143/137 (SIGTERM/SIGKILL) — the pylint run was killed
  * the .out does not end with a newline AND its last line is not a
    complete message/footer (mid-stream truncation)
  * the last non-empty line is a bare '************* Module X' header
    (a module was announced but its messages never streamed)

Prints one line per (corpus, profile); flags SUSPECT with the reason.
Such GTs cannot be used as authoritative for FP counting past the cut.
"""
import os, re, sys
ROOT = os.path.expanduser("~/Desktop/Projects/prylint")
RES = os.path.join(ROOT, "harness", "results")
CORPORA = ("scrapy rich werkzeug tornado celery fastapi black pip botocore mypy "
           "pydantic matplotlib ansible twisted sqlalchemy zulip django salt "
           "scikit-learn nova numpy sentry pandas sympy airflow pylfunc core").split()
HDR = re.compile(r"^\*{13} Module ")
MSG = re.compile(r"^[^:]+:\d+:\d+: [A-Z]\d{4}:")
FOOTER = re.compile(r"^Your code has been rated|^-+$|^$")

for c in CORPORA:
    for p in ("hook", "full"):
        out = os.path.join(RES, f"{c}.{p}.out")
        ex = os.path.join(RES, f"{c}.{p}.exit")
        if not os.path.exists(out):
            continue
        exitcode = open(ex).read().strip() if os.path.exists(ex) else "?"
        data = open(out, "rb").read()
        reasons = []
        if exitcode in ("143", "137"):
            reasons.append(f"exit={exitcode}(killed)")
        ends_nl = data.endswith(b"\n")
        text = data.decode("utf-8", "replace")
        lines = text.split("\n")
        # last non-empty line
        last = ""
        for ln in reversed(lines):
            if ln.strip():
                last = ln
                break
        if not ends_nl:
            # mid-stream cut unless it's a legitimate footerless last message
            if HDR.match(last):
                reasons.append("ends-on-bare-module-header(no-nl)")
            elif not MSG.match(last) and not FOOTER.match(last):
                reasons.append("no-trailing-newline-midline")
            elif MSG.match(last):
                reasons.append("last-msg-no-nl(maybe-ok)")
        else:
            if HDR.match(last):
                reasons.append("ends-on-bare-module-header")
        status = "SUSPECT" if any("killed" in r or "bare-module" in r or "midline" in r for r in reasons) else "ok"
        if status != "ok" or reasons:
            print(f"{c:14s} {p:4s} exit={exitcode:3s} {status:8s} {';'.join(reasons) if reasons else '-'}")
