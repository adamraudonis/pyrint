#!/usr/bin/env python3
"""Log EVERY infer of Subscript@orm/base.py:741:4 where the boundnode is a
ClassDef named SQLORMExpression — recording hit/miss, lk, cc-id, bn-id, ni,
result kinds. Reconcile the [Node] HIT (cachekey trace) vs [U] MINT (mint
trace) discrepancy by exposing the exact key tuple identity.

Usage: .venv-pylint/bin/python harness/trace_gt_741all.py <corpus> <flagsfile> 2> all.txt
"""
import sys, os
corpus = os.path.abspath(sys.argv[1]); flagsfile = os.path.abspath(sys.argv[2])
rcfile = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "empty.rcfile"))
os.chdir(corpus)

import astroid
from astroid import util
from astroid.nodes.node_ng import NodeNG

CUR = [""]
N = [0]

def is_target(self):
    if type(self).__name__ != "Subscript":
        return False
    if getattr(self, "lineno", None) != 741 or getattr(self, "col_offset", None) != 4:
        return False
    try:
        f = getattr(self.root(), "file", "") or ""
    except Exception:
        return False
    return "orm/base.py" in f

_orig = NodeNG.infer
def traced(self, context=None, **kw):
    if not is_target(self):
        yield from _orig(self, context, **kw)
        return
    bn = context.boundnode if context else None
    bnn = getattr(bn, "name", None) if bn is not None else None
    if bnn != "SQLORMExpression":
        yield from _orig(self, context, **kw)
        return
    lk = context.lookupname if context else None
    cc = context.callcontext if context else None
    key = (self, lk, cc, bn)
    hit = bool(context and key in context.inferred)
    ni = context.nodes_inferred if context else -1
    N[0] += 1
    if hit:
        cached = context.inferred[key]
        kinds = ",".join("U" if isinstance(r, util.UninferableBase) else type(r).__name__ for r in cached)
        sys.stderr.write(f"T741 #{N[0]} cur={CUR[0]} HIT lk={lk} cc={id(cc) if cc else None} bn_id={id(bn)} ni={ni} -> [{kinds}]\n")
        sys.stderr.flush()
        yield from _orig(self, context, **kw)
        return
    results = []
    for r in _orig(self, context, **kw):
        results.append(r); yield r
    kinds = ",".join("U" if isinstance(r, util.UninferableBase) else type(r).__name__ for r in results)
    sys.stderr.write(f"T741 #{N[0]} cur={CUR[0]} MINT lk={lk} cc={id(cc) if cc else None} bn_id={id(bn)} ni={ni} -> [{kinds}]\n")
    sys.stderr.flush()
NodeNG.infer = traced

import pylint.lint.pylinter as plr
_origcheck = plr.PyLinter.check_astroid_module
def wrapped_check(self, node, *a, **k):
    try:
        CUR[0] = os.path.basename(getattr(node.root(), "file", "") or "")
    except Exception:
        CUR[0] = "?"
    return _origcheck(self, node, *a, **k)
plr.PyLinter.check_astroid_module = wrapped_check

from pylint import lint
flags = [f.replace("HARNESS_EMPTY_RC", rcfile) for f in open(flagsfile).read().split()]
try:
    lint.Run(["."] + flags, exit=False)
except SystemExit:
    pass
sys.stderr.write(f"DONE pulls={N[0]}\n")
