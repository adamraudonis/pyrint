#!/usr/bin/env python3
"""Log EVERY infer() of the base.py:844:25 Mapped[_T_co] Subscript, globally,
with lk/cc/bn/ni and whether it HITS the inference cache or runs cold. Also log
the cache write (the tuple stored) so we can see whether astroid ever writes
[Uninferable] there and at what lk.

Usage: .venv-pylint/bin/python harness/trace_gt_sub844.py <corpus> <flagsfile> 2> t.txt
"""
import sys, os
corpus = os.path.abspath(sys.argv[1]); flagsfile = os.path.abspath(sys.argv[2])
rcfile = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "empty.rcfile"))
os.chdir(corpus)

import astroid
from astroid import nodes, util

def fname(n):
    try:
        return os.path.basename(n.root().file or "")
    except Exception:
        return "?"

def is_target(self):
    return (isinstance(self, nodes.Subscript)
            and getattr(self, "lineno", 0) == 844
            and getattr(self, "col_offset", -1) == 25
            and fname(self) == "base.py")

N = [0]
_orig_infer = nodes.NodeNG.infer
def traced_infer(self, context=None, **kw):
    if is_target(self) and N[0] < 60:
        # mirror astroid's own key computation
        from astroid.context import InferenceContext
        ctx = context if context is not None else None
        lk = ctx.lookupname if ctx else None
        bn = ctx.boundnode if ctx else None
        cc = ctx.callcontext if ctx else None
        ni = ctx.nodes_inferred if ctx else -1
        cached = False
        if ctx is not None:
            key = (self, ctx.lookupname, ctx.callcontext, ctx.boundnode)
            cached = key in ctx.inferred
        bns = getattr(bn, "name", type(bn).__name__) if bn is not None else "None"
        N[0] += 1
        res = list(_orig_infer(self, context, **kw))
        kinds = ",".join(type(r).__name__ if not isinstance(r, util.UninferableBase) else "U" for r in res)
        sys.stderr.write(
            f"SUB844 #{N[0]} lk={lk!r} cc={cc is not None} bn={bns} ni_in={ni} "
            f"cached={cached} file={os.path.basename(self.root().file or '')} -> [{kinds}]\n")
        sys.stderr.flush()
        yield from res
        return
    yield from _orig_infer(self, context, **kw)
nodes.NodeNG.infer = traced_infer

from pylint import lint
flags = [f.replace("HARNESS_EMPTY_RC", rcfile) for f in open(flagsfile).read().split()]
try:
    lint.Run(["."] + flags, exit=False)
except SystemExit:
    pass
sys.stderr.write("GTDONE\n")
