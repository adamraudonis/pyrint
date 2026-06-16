#!/usr/bin/env python3
"""GT equivalent of prylint's PRYLINT_DBG_DMW: log every global-cache write of
Subscript@741/@676 (orm/base.py) under boundnode=SQLORMExpression, with the
wipe count and current lint file. Mirrors the prylint DMW stream so the two can
be diffed to find where prylint's cache state diverges from astroid's.

Wraps NodeNG.infer: after fully draining a Subscript on line 741/676 whose
context.boundnode is the SQLORMExpression ClassDef, log a WRITE event with the
final nodes_inferred (to see truncation: cost ~0..2 = warm/small; >5 = recompute;
capped = truncated).

Usage: .venv-pylint/bin/python harness/trace_gt_dmw.py <corpus> <flagsfile> 2> t.txt
"""
import sys, os
corpus = os.path.abspath(sys.argv[1]); flagsfile = os.path.abspath(sys.argv[2])
rcfile = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "empty.rcfile"))
os.chdir(corpus)

import astroid
from astroid import nodes, context as actx, transforms as atr, util
from astroid.nodes.node_ng import NodeNG

_wipe = [0]
_oi = actx._invalidate_cache
def _ti():
    _wipe[0] += 1; return _oi()
actx._invalidate_cache = _ti
atr._invalidate_cache = _ti

CUR_FILE = [""]
import pylint.lint.pylinter as plinter
_orig_check = plinter.PyLinter._check_astroid_module
def traced_check(self, node, walker, rawcheckers, tokencheckers):
    CUR_FILE[0] = os.path.basename(getattr(node, "file", "") or "")
    return _orig_check(self, node, walker, rawcheckers, tokencheckers)
plinter.PyLinter._check_astroid_module = traced_check

_orig_infer = NodeNG.infer
def traced_infer(self, context=None, **kw):
    is_target = (isinstance(self, nodes.Subscript)
                 and getattr(self, "lineno", 0) in (741, 676)
                 and getattr(self.root(), "file", "").endswith("orm/base.py"))
    bn = getattr(context, "boundnode", None) if context else None
    is_sqlorm = bn is not None and getattr(bn, "name", "") == "SQLORMExpression"
    if not (is_target and is_sqlorm):
        yield from _orig_infer(self, context, **kw)
        return
    ni0 = context.nodes_inferred
    n = 0; cap = False
    for r in _orig_infer(self, context, **kw):
        n += 1
        if isinstance(r, util.UninferableBase): cap = True
        yield r
    ni1 = context.nodes_inferred
    sys.stderr.write(f"GTDMW Subscript@{self.lineno}#{id(self)%100000} cost={ni1-ni0} cap={cap} n={n} file={CUR_FILE[0]} wipe={_wipe[0]}\n")
NodeNG.infer = traced_infer

from pylint import lint
flags = [f.replace("HARNESS_EMPTY_RC", rcfile) for f in open(flagsfile).read().split()]
try:
    lint.Run(["."] + flags, exit=False)
except SystemExit:
    pass
sys.stderr.write(f"GTDONE total_wipes={_wipe[0]}\n")
