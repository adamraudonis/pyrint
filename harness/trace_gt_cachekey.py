#!/usr/bin/env python3
"""Log astroid's _INFERENCE_CACHE KEY (node, lookupname, callcontext, boundnode)
for every infer of a node in orm/base.py during the WARM MappedColumn W0231
check (full corpus). Flips a global logger ON only for the deciding
_DeclarativeMapped.igetattr('__init__') call so the noise is bounded.

Usage: .venv-pylint/bin/python harness/trace_gt_cachekey.py <corpus> <flagsfile> 2> ck.txt
"""
import sys, os
corpus = os.path.abspath(sys.argv[1]); flagsfile = os.path.abspath(sys.argv[2])
rcfile = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "empty.rcfile"))
os.chdir(corpus)

import astroid
from astroid import nodes, bases, util, context as actx
from astroid.nodes.node_ng import NodeNG
import pylint.checkers.classes.class_checker as cc

LOG = [False]
SEEN = [0]

def node_tag(n):
    try:
        f = os.path.basename(getattr(n.root(), "file", "") or "")
    except Exception:
        f = "?"
    ln = getattr(n, "lineno", None)
    col = getattr(n, "col_offset", None)
    return f"{type(n).__name__}@{f}:{ln}:{col} {n.repr_name()}"

_orig_infer = NodeNG.infer
def traced_infer(self, context=None, **kw):
    if not LOG[0]:
        yield from _orig_infer(self, context, **kw)
        return
    try:
        fn = os.path.basename(getattr(self.root(), "file", "") or "")
    except Exception:
        fn = "?"
    if fn != "base.py" and "_T_co" not in (self.repr_name() or "") and "TypeVar" not in (self.repr_name() or ""):
        yield from _orig_infer(self, context, **kw)
        return
    ln = getattr(self, "lineno", None)
    # only orm/base.py + _T_co-ish nodes
    if fn == "base.py":
        try:
            full = getattr(self.root(), "file", "")
        except Exception:
            full = ""
        if "orm/base.py" not in full and "orm\\base.py" not in full:
            yield from _orig_infer(self, context, **kw)
            return
    lk = context.lookupname if context else None
    cc_ = id(context.callcontext) if (context and context.callcontext) else None
    bn = context.boundnode if context else None
    bnr = node_tag(bn) if bn is not None else None
    ni = context.nodes_inferred if context else -1
    key = (self, lk, context.callcontext if context else None, bn)
    hit = bool(context and key in context.inferred)
    SEEN[0] += 1
    sys.stderr.write(f"CK {node_tag(self)} lk={lk} cc={cc_} bn={bnr} ni={ni} hit={hit}\n")
    sys.stderr.flush()
    yield from _orig_infer(self, context, **kw)
NodeNG.infer = traced_infer

_orig_anc = cc._ancestors_to_call
def traced_anc(klass_node, method_name="__init__"):
    cname = getattr(klass_node, "name", "?")
    if cname != "MappedColumn":
        return _orig_anc(klass_node, method_name)
    to_call = {}
    for base_node in klass_node.ancestors(recurs=False):
        bn = getattr(base_node, "name", "?")
        on = bn == "_DeclarativeMapped"
        if on:
            sys.stderr.write(f"=== igetattr __init__ on {bn} (LOG ON) ===\n")
            LOG[0] = True
        try:
            init_node = next(base_node.igetattr(method_name))
            if not isinstance(init_node, astroid.UnboundMethod):
                kind = type(init_node).__name__
            else:
                isab = init_node.is_abstract()
                kind = f"UM(abstract={isab}, frame={getattr(getattr(init_node,'parent',None),'name','?')})"
                if not init_node.is_abstract():
                    to_call[base_node] = init_node
        except astroid.InferenceError:
            kind = "InferenceError"
        if on:
            LOG[0] = False
            sys.stderr.write(f"=== {bn} __init__ -> {kind} (LOG OFF, {SEEN[0]} infers logged) ===\n")
            sys.stderr.flush()
    sys.stderr.write(f"  => to_call keys: {[getattr(k,'name','?') for k in to_call]}\n")
    return to_call
cc._ancestors_to_call = traced_anc

from pylint import lint
flags = [f.replace("HARNESS_EMPTY_RC", rcfile) for f in open(flagsfile).read().split()]
try:
    lint.Run(["."] + flags, exit=False)
except SystemExit:
    pass
