#!/usr/bin/env python3
"""Measure astroid's COLD cost of the _DeclarativeMapped.igetattr('__init__')
W0231 chain on a single isolated file (fresh _INFERENCE_CACHE). Patches
_ancestors_to_call to, for MappedColumn, run each base's igetattr in a FRESH
InferenceContext and report nodes_inferred consumed + result kind.

Usage: .venv-pylint/bin/python harness/trace_gt_declmeta_cost.py <file.py> 2> c.txt
"""
import sys, os
target = os.path.abspath(sys.argv[1])
rcfile = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "empty.rcfile"))

import astroid
from astroid import nodes, bases, util, context as actx
import pylint.checkers.classes.class_checker as cc

_orig = cc._ancestors_to_call
def traced(klass_node, method_name="__init__"):
    cname = getattr(klass_node, "name", "?")
    if cname not in ("MappedColumn", "QueryableAttribute"):
        return _orig(klass_node, method_name)
    to_call = {}
    for base_node in klass_node.ancestors(recurs=False):
        bn = getattr(base_node, "name", "?")
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
            sys.stderr.write(f"COLD {cname} base={bn} ni={ni} -> {kind}\n")
        except astroid.InferenceError:
            sys.stderr.write(f"COLD {cname} base={bn} ni={ctx.nodes_inferred} -> InferenceError\n")
        sys.stderr.flush()
    return _orig(klass_node, method_name)
cc._ancestors_to_call = traced

from pylint import lint
try:
    lint.Run([target, f"--rcfile={rcfile}", "--disable=all", "--enable=W0231"], exit=False)
except SystemExit:
    pass
