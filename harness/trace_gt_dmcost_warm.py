#!/usr/bin/env python3
"""WARM full-corpus cost of _DeclarativeMapped.igetattr('__init__') at the
MappedColumn / QueryableAttribute W0231 check. Reports the REAL nodes_inferred
consumed by the fresh-context igetattr (astroid's _ancestors_to_call makes a
fresh context per base) AND the result kind, at the warm moment.

Usage: .venv-pylint/bin/python harness/trace_gt_dmcost_warm.py <corpus> <flagsfile> 2> w.txt
"""
import sys, os
corpus = os.path.abspath(sys.argv[1]); flagsfile = os.path.abspath(sys.argv[2])
rcfile = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "empty.rcfile"))
os.chdir(corpus)

import astroid
from astroid import nodes, bases, util, context as actx
import pylint.checkers.classes.class_checker as cc

_orig = cc._ancestors_to_call
def traced(klass_node, method_name="__init__"):
    cname = getattr(klass_node, "name", "?")
    if cname not in ("MappedColumn", "QueryableAttribute"):
        return _orig(klass_node, method_name)
    try:
        fn = os.path.basename(getattr(klass_node.root(), "file", "") or "")
    except Exception:
        fn = "?"
    to_call = {}
    for base_node in klass_node.ancestors(recurs=False):
        bn = getattr(base_node, "name", "?")
        # mirror astroid: fresh context (igetattr with no context)
        ctx = actx.InferenceContext()
        try:
            init_node = next(base_node.igetattr(method_name, context=ctx))
            ni = ctx.nodes_inferred
            if isinstance(init_node, util.UninferableBase):
                kind = "U"
            elif isinstance(init_node, bases.UnboundMethod):
                isab = init_node.is_abstract()
                kind = f"UM(abstract={isab}, frame={getattr(init_node.parent,'name','?')})"
                if not init_node.is_abstract():
                    to_call[base_node] = init_node
            else:
                kind = type(init_node).__name__
            sys.stderr.write(f"WARM {cname}@{fn} base={bn} ni={ni} -> {kind}\n")
        except astroid.InferenceError:
            sys.stderr.write(f"WARM {cname}@{fn} base={bn} ni={ctx.nodes_inferred} -> InferenceError\n")
        sys.stderr.flush()
    return _orig(klass_node, method_name)
cc._ancestors_to_call = traced

from pylint import lint
flags = [f.replace("HARNESS_EMPTY_RC", rcfile) for f in open(flagsfile).read().split()]
try:
    lint.Run(["."] + flags, exit=False)
except SystemExit:
    pass
