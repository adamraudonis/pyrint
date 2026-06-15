#!/usr/bin/env python3
"""Trace astroid's LINT-PATH inference of E1120 call funcs in a real pylint run.

Monkeypatches pylint.checkers.typecheck's safe_infer reference so that every
visit_call's `safe_infer(node.func, compare_constructors=True)` logs the call
site (file:line:col), the func's last attrname, the inferred result type, and
context.nodes_inferred. This reveals, for the actual full-corpus lint run,
whether astroid resolves a decorated-classmethod func to UnboundMethod (warm ->
E1120) or Uninferable (cold-truncated -> no check) at each site.

Usage: .venv-pylint/bin/python harness/trace_gt_e1120.py <corpus_dir> <flagsfile>
       [--name=get_by_uuid]   # only log funcs whose attrname == this
2> trace_e1120.txt
Filtered by GT_E1120_NAME env or --name.
"""
import sys, os

name_filter = None
args = []
for a in sys.argv[1:]:
    if a.startswith("--name="):
        name_filter = a.split("=", 1)[1]
    else:
        args.append(a)
name_filter = name_filter or os.environ.get("GT_E1120_NAME")

corpus = os.path.abspath(args[0])
flagsfile = os.path.abspath(args[1])
rcfile = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "empty.rcfile"))

os.chdir(corpus)

import astroid
from astroid import context as actx
import pylint.checkers.typecheck as tc

orig_safe_infer = tc.safe_infer


def traced_safe_infer(node, *a, **kw):
    ctx = actx.InferenceContext()
    # mimic safe_infer: it builds its own context internally; we can't see its
    # ni. Instead call the real one, then separately probe with a fresh ctx to
    # read ni. But to avoid double inference cost differences we instead just
    # run the real safe_infer and inspect the result.
    res = orig_safe_infer(node, *a, **kw)
    try:
        attrname = getattr(node, "attrname", None) or getattr(node, "name", None) or ""
    except Exception:
        attrname = "?"
    if name_filter is None or attrname == name_filter:
        # determine result kind
        from astroid import bases, nodes, util
        if res is None:
            kind = "None"
        elif isinstance(res, util.UninferableBase):
            kind = "U"
        elif isinstance(res, bases.UnboundMethod) and not isinstance(res, bases.BoundMethod):
            kind = "UM"
        elif isinstance(res, bases.BoundMethod):
            kind = "BM"
        else:
            kind = type(res).__name__
        root = node.root()
        fn = getattr(root, "file", "?")
        if fn:
            fn = fn.replace(os.getcwd() + os.sep, "")
        sys.stderr.write(f"GTE1120 {attrname} {fn}:{node.fromlineno}:{node.col_offset} -> {kind} wipes={_wipe_count[0]}\n")
        stop = os.environ.get("GT_E1120_STOPFILE")
        if stop and stop in fn:
            sys.stderr.write(f"GTSTOP reached {fn} wipes={_wipe_count[0]}\n")
            sys.stderr.flush()
            os._exit(0)
    return res


tc.safe_infer = traced_safe_infer

# count + log cache wipes (transforms._invalidate_cache)
from astroid import context as _actx
from astroid import transforms as _atr
_wipe_count = [0]
_orig_inval = _actx._invalidate_cache


def _traced_inval():
    _wipe_count[0] += 1
    if os.environ.get("GT_E1120_WIPES"):
        sys.stderr.write(f"GTWIPE #{_wipe_count[0]}\n")
    return _orig_inval()


_actx._invalidate_cache = _traced_inval
_atr._invalidate_cache = _traced_inval

# run pylint
from pylint import lint
flags = open(flagsfile).read().split()
flags = [f.replace("HARNESS_EMPTY_RC", rcfile) for f in flags]
try:
    lint.Run(["."] + flags, exit=False)
except SystemExit:
    pass
