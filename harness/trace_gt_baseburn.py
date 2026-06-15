#!/usr/bin/env python3
"""Observe (WITHOUT double-executing) astroid's per-base infer cost inside
declared_metaclass for _DeclarativeMapped, plus the getattr ancestors result,
in the warm full run. Logs the cache key (lookupname/cc-id/bn-id) and result
kind for each base subscript so we can see which entry is warm.

Usage: PYTHONHASHSEED=0 .venv-pylint/bin/python harness/trace_gt_baseburn.py <corpus> <flagsfile> 2>t.txt
"""
import sys, os
corpus = os.path.abspath(sys.argv[1]); flagsfile = os.path.abspath(sys.argv[2])
rcfile = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "empty.rcfile"))
os.chdir(corpus)

import astroid
from astroid import nodes, bases, util
from astroid import context as actx

def kind(r):
    if isinstance(r, util.UninferableBase): return "U"
    if isinstance(r, nodes.ClassDef): return f"Class:{r.name}"
    if isinstance(r, bases.Instance): return f"Inst:{r._proxied.name}"
    return type(r).__name__

# Wrap declared_metaclass to observe the REAL base.infer for _DeclarativeMapped.
_orig_dm = nodes.ClassDef.declared_metaclass
def traced_dm(self, context=None):
    if self.name == "_DeclarativeMapped" and context is not None:
        ln = context.lookupname
        for base in self.bases:
            ckey = (base, context.lookupname, context.callcontext, context.boundnode)
            hit = ckey in actx._INFERENCE_CACHE
            cached = actx._INFERENCE_CACHE.get(ckey)
            ck = [kind(r) for r in cached] if cached is not None else None
            sys.stderr.write(
                f"DMBASE base@{base.col_offset} ni={context.nodes_inferred} ln={ln} "
                f"cc={id(context.callcontext) if context.callcontext else None} "
                f"bn={id(context.boundnode) if context.boundnode else None} "
                f"hit={hit} cached={ck}\n")
        sys.stderr.flush()
    return _orig_dm(self, context=context)
nodes.ClassDef.declared_metaclass = traced_dm

from pylint import lint
flags = [f.replace("HARNESS_EMPTY_RC", rcfile) for f in open(flagsfile).read().split()]
try:
    lint.Run(["."] + flags, exit=False)
except SystemExit:
    pass
