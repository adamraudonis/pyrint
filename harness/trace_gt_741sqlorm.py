#!/usr/bin/env python3
"""Find every COLD MINT (cache write) of (Subscript@orm/base.py:741:4, lk=None,
cc=None, bn=SQLORMExpression) across the full corpus, and report the file +
the ni at mint + the result kind. Identifies WHERE/WHEN astroid first warms the
deciding entry to a resolved class so prylint hits it warm at MappedColumn.

Usage: .venv-pylint/bin/python harness/trace_gt_741sqlorm.py <corpus> <flagsfile> 2> m.txt
"""
import sys, os
corpus = os.path.abspath(sys.argv[1]); flagsfile = os.path.abspath(sys.argv[2])
rcfile = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "empty.rcfile"))
os.chdir(corpus)

import astroid
from astroid import nodes, util
from astroid.nodes.node_ng import NodeNG

CUR = [""]
COUNT = [0]

def is_target(self):
    if type(self).__name__ != "Subscript":
        return False
    if getattr(self, "lineno", None) != 741 or getattr(self, "col_offset", None) != 4:
        return False
    try:
        f = getattr(self.root(), "file", "") or ""
    except Exception:
        return False
    return "orm/base.py" in f or "orm\\base.py" in f

_orig = NodeNG.infer
def traced(self, context=None, **kw):
    if not is_target(self):
        yield from _orig(self, context, **kw)
        return
    bn = context.boundnode if context else None
    lk = context.lookupname if context else None
    cc = context.callcontext if context else None
    bnn = getattr(bn, "name", None) if bn is not None else None
    if bnn != "SQLORMExpression" or lk is not None or cc is not None:
        yield from _orig(self, context, **kw)
        return
    key = (self, lk, cc, bn)
    hit = bool(context and key in context.inferred)
    ni = context.nodes_inferred if context else -1
    results = []
    for r in _orig(self, context, **kw):
        results.append(r)
        yield r
    kinds = ",".join("U" if isinstance(r, util.UninferableBase) else type(r).__name__ for r in results)
    COUNT[0] += 1
    if not hit:  # only log MINTS (misses that compute)
        sys.stderr.write(f"MINT741SQLORM #{COUNT[0]} cur={CUR[0]} ni={ni} -> [{kinds}]\n")
        sys.stderr.flush()
NodeNG.infer = traced

# track the file currently being linted
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
sys.stderr.write(f"DONE total target pulls={COUNT[0]}\n")
