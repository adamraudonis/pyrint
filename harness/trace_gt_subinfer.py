#!/usr/bin/env python3
"""During the cold _DeclarativeMapped.declared_metaclass(lk='__init__') window,
log EVERY Subscript.infer (base.py subscripts) with bn/cc/lk and whether it is a
PATH-GUARD short-circuit (already on context.path) or a cache HIT or a full
re-walk. This reveals WHY astroid's bn=set declared_metaclass(SQLCoreOperations)
does NOT re-infer _T_co (path-guard vs cache vs early-return).

Usage: .venv-pylint/bin/python harness/trace_gt_subinfer.py <corpus> <flagsfile> 2> t.txt
"""
import sys, os
corpus = os.path.abspath(sys.argv[1]); flagsfile = os.path.abspath(sys.argv[2])
rcfile = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "empty.rcfile"))
os.chdir(corpus)

import astroid
from astroid import nodes, util
from astroid.context import _INFERENCE_CACHE

ARMED = [0]
DONE = [False]
LOG = []

def fbase(n):
    try:
        return os.path.basename(n.root().file or "")
    except Exception:
        return "?"

_orig_dm = nodes.ClassDef.declared_metaclass
def traced_dm(self, context=None):
    lk = context.lookupname if context else None
    arm = (getattr(self, "name", "") == "_DeclarativeMapped" and lk == "__init__" and not DONE[0])
    if arm:
        ARMED[0] += 1
    try:
        return _orig_dm(self, context)
    finally:
        if arm:
            ARMED[0] -= 1
            if ARMED[0] == 0:
                DONE[0] = True
                sys.stderr.write("SUBINFER-WINDOW:\n" + "\n".join(LOG) + "\n")
                sys.stderr.flush()
nodes.ClassDef.declared_metaclass = traced_dm

_orig_infer = nodes.NodeNG.infer
def traced_infer(self, context=None, **kw):
    if ARMED[0] > 0 and not DONE[0] and isinstance(self, nodes.Subscript) and fbase(self) == "base.py":
        bn = context.boundnode if context else None
        bns = getattr(bn, "name", type(bn).__name__) if bn is not None else "None"
        lk = context.lookupname if context else None
        cc = (context.callcontext is not None) if context else False
        ni = context.nodes_inferred if context else -1
        # path-guard check: _infer is path_wrapped on (self, lookupname)
        on_path = bool(context and (self, context.lookupname) in context.path)
        key = (self, context.lookupname, context.callcontext, context.boundnode) if context else None
        cache_hit = key in _INFERENCE_CACHE if context else False
        LOG.append(f"  SUB @{self.lineno}:{self.col_offset} '{self.as_string()[:18]}' bn={bns} cc={cc} lk={lk!r} ni={ni} on_path={on_path} cache_hit={cache_hit}")
    yield from _orig_infer(self, context, **kw)
nodes.NodeNG.infer = traced_infer

from pylint import lint
flags = [f.replace("HARNESS_EMPTY_RC", rcfile) for f in open(flagsfile).read().split()]
try:
    lint.Run(["."] + flags, exit=False)
except SystemExit:
    pass
sys.stderr.write("GTDONE\n")
