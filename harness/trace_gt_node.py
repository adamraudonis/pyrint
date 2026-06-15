#!/usr/bin/env python3
"""Trace astroid's NodeNG.infer cache HIT/MISS for a target node (file:line:col)
in the warm full run, showing nodes_inferred + cached result kinds. Reveals
whether astroid caches an over-cap [Uninferable] for a given base subscript.

Usage: GT_NODE=844:18,844:40 GT_FILE=orm/base.py \
  .venv-pylint/bin/python harness/trace_gt_node.py <corpus> <flagsfile> 2> t.txt
"""
import sys, os
corpus = os.path.abspath(sys.argv[1]); flagsfile = os.path.abspath(sys.argv[2])
rcfile = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "empty.rcfile"))
os.chdir(corpus)

targets = set((os.environ.get("GT_NODE") or "").split(","))
filesub = os.environ.get("GT_FILE", "")

import astroid
from astroid import nodes, bases, util
from astroid.nodes.node_ng import NodeNG
from astroid.context import InferenceContext
from astroid.manager import AstroidManager

def kind(r):
    if isinstance(r, util.UninferableBase): return "U"
    if isinstance(r, nodes.ClassDef): return f"Class:{r.name}"
    if isinstance(r, bases.Instance): return f"Inst:{r._proxied.name}"
    return type(r).__name__

_orig = NodeNG.infer
def traced(self, context=None, **kw):
    try:
        fn = getattr(self.root(), "file", "") or ""
    except Exception:
        fn = ""
    key = f"{getattr(self,'fromlineno','?')}:{getattr(self,'col_offset','?')}"
    on = (key in targets) and (filesub in fn)
    if not on:
        yield from _orig(self, context=context, **kw)
        return
    from astroid import context as _ac
    ctx = context or InferenceContext()
    ckey = (self, ctx.lookupname, ctx.callcontext, ctx.boundnode)
    hit = ckey in _ac._INFERENCE_CACHE
    if hit:
        cached = _ac._INFERENCE_CACHE[ckey]
        sys.stderr.write(f"GTNODE HIT {type(self).__name__} {key} ln={ctx.lookupname} ni={ctx.nodes_inferred} -> [{','.join(kind(r) for r in cached)}]\n")
    else:
        sys.stderr.write(f"GTNODE MISS {type(self).__name__} {key} ln={ctx.lookupname} ni_in={ctx.nodes_inferred}\n")
    sys.stderr.flush()
    out = []
    for r in _orig(self, context=context, **kw):
        out.append(kind(r)); yield r
    if not hit:
        sys.stderr.write(f"GTNODE  ...{type(self).__name__} {key} produced [{','.join(out)}] cached_now={ckey in _ac._INFERENCE_CACHE}\n")
        sys.stderr.flush()
NodeNG.infer = traced

from pylint import lint
flags = [f.replace("HARNESS_EMPTY_RC", rcfile) for f in open(flagsfile).read().split()]
try:
    lint.Run(["."] + flags, exit=False)
except SystemExit:
    pass
