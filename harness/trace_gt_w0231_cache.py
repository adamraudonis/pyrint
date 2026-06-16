#!/usr/bin/env python3
"""At the WARM MappedColumn W0231 check, dump astroid's _INFERENCE_CACHE entries
keyed on _DeclarativeMapped's base nodes (the `Mapped[_T_co]` Subscript), to see
whether astroid holds the same poisoned `(base, "__init__", None, None) = (U,)`
entry prylint does. Also report total wipes and whether the base infers to a
ClassDef reaching object under the warm ctx with lookupname='__init__'.

Usage: .venv-pylint/bin/python harness/trace_gt_w0231_cache.py <corpus> <flagsfile> 2> t.txt
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

def desc_results(t):
    out = []
    for r in t:
        if isinstance(r, util.UninferableBase):
            out.append("U")
        elif isinstance(r, nodes.ClassDef):
            out.append("Class:" + r.name)
        else:
            out.append(type(r).__name__)
    return out

_orig = cc._ancestors_to_call
def traced(klass_node, method_name="__init__"):
    cname = getattr(klass_node, "name", "?")
    if cname != "MappedColumn":
        return _orig(klass_node, method_name)
    sys.stderr.write(f"GTW0231CACHE class={cname} wipes={_wipe[0]}\n")
    # find the _DeclarativeMapped base and dump its base-node cache entries
    for base_node in klass_node.ancestors(recurs=False):
        if getattr(base_node, "name", "") != "_DeclarativeMapped":
            continue
        sys.stderr.write(f"  found _DeclarativeMapped; bases:\n")
        for bstmt in base_node.bases:
            sys.stderr.write(f"    base stmt={bstmt.as_string()!r} id={id(bstmt)}\n")
            for k, v in actx._INFERENCE_CACHE.items():
                if k[0] is bstmt:
                    sys.stderr.write(
                        f"      CACHE lk={k[1]!r} cc={k[2] is not None} bn={k[3] is not None} -> {desc_results(v)}\n"
                    )
        # Now run the EXACT igetattr astroid's check runs and report
        try:
            init_node = next(base_node.igetattr(method_name))
            if isinstance(init_node, util.UninferableBase):
                kind = "U"
            elif isinstance(init_node, bases.UnboundMethod):
                kind = f"UM(abstract={init_node.is_abstract()}, frame={getattr(getattr(init_node,'parent',None),'name','?')})"
            else:
                kind = type(init_node).__name__
        except astroid.InferenceError:
            kind = "InferenceError"
        sys.stderr.write(f"  igetattr __init__ -> {kind}\n")
    sys.stderr.flush()
    return _orig(klass_node, method_name)
cc._ancestors_to_call = traced

from pylint import lint
flags = [f.replace("HARNESS_EMPTY_RC", rcfile) for f in open(flagsfile).read().split()]
try:
    lint.Run(["."] + flags, exit=False)
except SystemExit:
    pass
sys.stderr.write(f"GTDONE wipes={_wipe[0]}\n")
