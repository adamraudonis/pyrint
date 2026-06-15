#!/usr/bin/env python3
"""Trace astroid's WARM lint-path _ancestors_to_call for the sqlalchemy W0231 FP.

Patches pylint.checkers.classes.class_checker._ancestors_to_call to log, for
the MappedColumn / QueryableAttribute / Proxy classes, every base + the
igetattr('__init__') result kind + is_abstract, at the real full-corpus warm
moment the W0231 check runs.

Usage: .venv-pylint/bin/python harness/trace_gt_w0231.py <corpus> <flagsfile> 2> t.txt
"""
import sys, os
corpus = os.path.abspath(sys.argv[1]); flagsfile = os.path.abspath(sys.argv[2])
rcfile = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "empty.rcfile"))
os.chdir(corpus)

import astroid
from astroid import nodes, bases, util, context as actx, transforms as atr
import pylint.checkers.classes.class_checker as cc

_wipe = [0]
_oi = actx._invalidate_cache
def _ti():
    _wipe[0] += 1; return _oi()
actx._invalidate_cache = _ti; atr._invalidate_cache = _ti

FP = {"MappedColumn", "QueryableAttribute", "Proxy", "array"}
_orig = cc._ancestors_to_call
def traced(klass_node, method_name="__init__"):
    cname = getattr(klass_node, "name", "?")
    if cname not in FP:
        return _orig(klass_node, method_name)
    try:
        fn = os.path.basename(getattr(klass_node.root(), "file", "") or "")
    except Exception:
        fn = "?"
    sys.stderr.write(f"GTW0231 class={cname} {fn} wipes={_wipe[0]}\n")
    to_call = {}
    for base_node in klass_node.ancestors(recurs=False):
        bn = getattr(base_node, "name", "?")
        try:
            init_node = next(base_node.igetattr(method_name))
            if isinstance(init_node, util.UninferableBase):
                kind = "U"
            elif isinstance(init_node, bases.UnboundMethod):
                isab = init_node.is_abstract()
                kind = f"UM(abstract={isab}, frame={getattr(init_node, 'parent', None) and getattr(getattr(init_node,'parent',None),'name','?')})"
                if isinstance(init_node, astroid.UnboundMethod) and not init_node.is_abstract():
                    to_call[base_node] = init_node
            else:
                kind = type(init_node).__name__
        except astroid.InferenceError:
            kind = "InferenceError"
        sys.stderr.write(f"  base {bn}: __init__ -> {kind}\n")
    sys.stderr.write(f"  => to_call keys: {[getattr(k,'name','?') for k in to_call]}\n")
    sys.stderr.flush()
    return to_call
cc._ancestors_to_call = traced

from pylint import lint
flags = [f.replace("HARNESS_EMPTY_RC", rcfile) for f in open(flagsfile).read().split()]
try:
    lint.Run(["."] + flags, exit=False)
except SystemExit:
    pass
sys.stderr.write(f"GTDONE wipes={_wipe[0]}\n")
