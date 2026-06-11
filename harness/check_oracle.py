#!/usr/bin/env python3
"""Differential unit test for the EMBEDDED stdlib-only syntax oracle
(crates/cli/src/syntax_oracle.py -- the file include_str!'d into the binary)
against the astroid-backed reference oracle (harness/syntax_oracle.py) run on
the pinned .venv-pylint interpreter.

Inputs:
  1. Every direct-E0001 file across ALL corpora, enumerated from the
     harness/results/*.iso.out ground truth ("Parsing failed" + tokenize-form
     lines; "Cannot import" lines point at importers, whose broken targets
     are themselves corpus files already enumerated).
  2. Synthetic edge cases (null bytes, unknown/wrong encodings, BOM clash,
     missing file, .pyi sibling redirect, type-comment retry both arms,
     py3.14-only syntax, empty modname).

Both oracles get identical JSON-lines requests; verdicts must be identical
as parsed JSON. Exit 0 = all match.
"""

import json
import os
import re
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
VENV_PY = os.path.join(ROOT, ".venv-pylint", "bin", "python")
OLD = os.path.join(ROOT, "harness", "syntax_oracle.py")
NEW = os.path.join(ROOT, "crates", "cli", "src", "syntax_oracle.py")

CORPORA = (
    "django pandas salt airflow core sentry pylfunc scrapy celery pip fastapi "
    "sqlalchemy numpy scikit-learn matplotlib ansible sympy rich tornado "
    "werkzeug black botocore mypy pydantic twisted nova zulip"
).split()

MODULE_RE = re.compile(r"^\*+ Module (.+)$")
E0001_RE = re.compile(r"^(.*?):\d+:\d+: E0001: ")


def corpus_requests():
    reqs = []
    for c in CORPORA:
        iso = os.path.join(ROOT, "harness", "results", f"{c}.iso.out")
        if not os.path.exists(iso):
            print(f"WARNING: missing ground truth {iso}", file=sys.stderr)
            continue
        module = None
        seen = set()
        with open(iso, encoding="utf-8", errors="replace") as f:
            for line in f:
                m = MODULE_RE.match(line)
                if m:
                    module = m.group(1)
                    continue
                m = E0001_RE.match(line)
                if not m or "Cannot import " in line:
                    continue
                rel = m.group(1)
                path = os.path.join(ROOT, "corpora", c, rel)
                if path in seen:
                    continue
                seen.add(path)
                reqs.append({"path": path, "modname": module})
    return reqs


def synthetic_requests(tmp):
    def w(name, data):
        p = os.path.join(tmp, name)
        mode = "wb" if isinstance(data, bytes) else "w"
        with open(p, mode) as f:
            f.write(data)
        return p

    reqs = []

    def add(path, modname):
        reqs.append({"path": path, "modname": modname})

    add(w("null_bytes.py", b"x = 1\n\x00\n"), "null_bytes")
    add(w("unknown_enc.py", b"# -*- coding: IBO-8859-1 -*-\nx = 1\n"), "unknown_enc")
    # utf-8 BOM clashing with an explicit non-utf-8 coding declaration
    add(w("bom_clash.py", b"\xef\xbb\xbf# -*- coding: latin-1 -*-\nx = 1\n"), "bom_clash")
    # declared ascii but non-ascii bytes -> UnicodeError while reading
    add(w("wrong_enc.py", b"# -*- coding: ascii -*-\nx = '\xff\xfe'\n"), "wrong_enc")
    add(os.path.join(tmp, "does_not_exist.py"), "does_not_exist")
    # .pyi with broken syntax + healthy sibling .py: get_source_file redirect
    w("sibling.py", "x = 1\n")
    add(w("sibling.pyi", "def f(:\n"), "sibling")
    # .pyi with broken syntax, no sibling
    add(w("lonely.pyi", "def f(:\n"), "lonely")
    # plain syntax error (modname embedded in message)
    add(w("plain_err.py", "def f(:\n"), "pkg.sub.plain_err")
    # same with empty modname -> '(<unknown>, line N)'
    add(w("plain_err2.py", "def f(:\n"), "")
    # misplaced type comment: parse with type_comments=True fails, the retry
    # without type comments succeeds -> ok
    add(w("type_retry_ok.py", "x = 1 # type: int # type: int\n"), "type_retry_ok")
    # line contains '# type:' AND is broken regardless: retry fails too ->
    # '(<unknown>, line N)'
    add(w("type_retry_fail.py", "def f(x y): # type: int\n    pass\n"), "type_retry_fail")
    # py3.14-only syntax on the pinned 3.12 interpreter
    add(w("tstring.py", 'name = "x"\nt = t"hello {name}"\n'), "tstring")
    # parses, but tokenize.TokenError (trailing line continuation)
    add(w("tok_err.py", '""\\\n'), "tok_err")
    # healthy file -> plain ok
    add(w("healthy.py", "import os\nprint(os.sep)\n"), "healthy")
    # astroid TreeRebuilder RecursionError boundary (deep UNPARENTHESIZED
    # chains; pinned astroid 4.0.4 / CPython 3.12 first crashes at 494
    # levels): both sides of the boundary, per chain construct. The embedded
    # oracle emulates the rebuilder's 2-frames-per-level recursion and is
    # shim-calibrated; these cases pin the calibration differentially.
    chains = {
        "binop": lambda n: "x = " + "+".join(["1"] * (n + 1)) + "\n",
        "unary": lambda n: "x = " + "-" * n + "1\n",
        "attr": lambda n: "import os\nx = os" + ".__class__" * n + "\n",
        "ifexp": lambda n: "x = " + "1 if 1 else " * n + "1\n",
        "lam": lambda n: "x = " + "lambda: " * n + "1\n",
    }
    for kind, gen in chains.items():
        for n in (493, 494):
            add(w(f"deep_{kind}_{n}.py", gen(n)), f"deep_{kind}_{n}")
    # parenthesized nesting is capped by CPython's parser itself (~200):
    # exact SyntaxError parity, no rebuilder involvement
    add(w("deep_call.py", "f = min\nx = " + "f(" * 300 + "1" + ")" * 300 + "\n"), "deep_call")
    add(w("deep_list.py", "x = " + "[" * 300 + "1" + "]" * 300 + "\n"), "deep_list")

    # 3-frames-per-level statement nesting (astroid's _visit_with /
    # _visit_functiondef helpers and the _visit_dict_items generator):
    # sweep a binop chain nested inside them across the predicted boundary
    # so any frame-cost mismatch in the emulation shows as a differential.
    def chain(n):
        return "+".join(["1"] * (n + 1))

    def in_withs(wd, n):
        lines = [" " * i + "with c:" for i in range(wd)]
        lines.append(" " * wd + "x = " + chain(n))
        return "\n".join(lines) + "\n"

    def in_defs(d, n):
        lines = [" " * i + f"def f{i}():" for i in range(d)]
        lines.append(" " * d + "x = " + chain(n))
        return "\n".join(lines) + "\n"

    def in_dicts(k, n):
        return "x = " + "{1: " * k + chain(n) + "}" * k + "\n"

    for n in range(412, 426):  # 50 withs: ~494 - 50*3/2 = ~419
        add(w(f"with50_{n}.py", in_withs(50, n)), f"with50_{n}")
    for n in range(442, 456):  # 30 defs: ~494 - 30*3/2 = ~449
        add(w(f"def30_{n}.py", in_defs(30, n)), f"def30_{n}")
    for n in range(338, 352):  # 100 dicts: ~494 - 100*3/2 = ~344
        add(w(f"dict100_{n}.py", in_dicts(100, n)), f"dict100_{n}")
    return reqs


def run_oracle(cmd, reqs):
    inp = "".join(json.dumps(r) + "\n" for r in reqs)
    proc = subprocess.run(
        cmd, input=inp, capture_output=True, text=True, check=False
    )
    lines = [l for l in proc.stdout.splitlines() if l.strip()]
    if len(lines) != len(reqs):
        print(
            f"FATAL: {cmd} returned {len(lines)}/{len(reqs)} verdicts; "
            f"stderr:\n{proc.stderr}",
            file=sys.stderr,
        )
        sys.exit(2)
    return [json.loads(l) for l in lines]


def main():
    if not os.path.exists(VENV_PY):
        print(f"FATAL: pinned interpreter missing: {VENV_PY}", file=sys.stderr)
        sys.exit(2)
    reqs = corpus_requests()
    n_corpus = len(reqs)
    with tempfile.TemporaryDirectory() as tmp:
        reqs += synthetic_requests(tmp)
        old = run_oracle([VENV_PY, OLD], reqs)
        new = run_oracle([VENV_PY, "-I", NEW], reqs)
        bad = 0
        for r, o, n in zip(reqs, old, new):
            if o != n:
                bad += 1
                print(f"MISMATCH {r['path']} (modname={r['modname']!r})")
                print(f"  old: {json.dumps(o)}")
                print(f"  new: {json.dumps(n)}")
    print(
        f"check_oracle: {len(reqs)} requests ({n_corpus} corpus E0001 files + "
        f"{len(reqs) - n_corpus} synthetic), {bad} mismatches"
    )
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
