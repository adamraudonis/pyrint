#!/usr/bin/env python3
"""Trace the SOURCE of astroid cache wipes during a real pylint run, gated to
phase 2 (after the first analyzed file's checks begin) and optionally to a
window of files. Each wipe is attributed to the transform function + node.

Usage: .venv-pylint/bin/python harness/trace_gt_wipesrc.py <corpus> <flagsfile>
Env: GT_WS_FROM=<substr>  start counting/logging when this file is first checked
     GT_WS_STOP=<substr>  os._exit after this file is checked
     GT_WS_TOPN=<n>       at stop, print top-N transform-fn wipe sources
"""
import sys, os, collections

args = [a for a in sys.argv[1:] if not a.startswith("--")]
corpus = os.path.abspath(args[0])
flagsfile = os.path.abspath(args[1])
rcfile = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "empty.rcfile"))
os.chdir(corpus)

FROM = os.environ.get("GT_WS_FROM", "")
STOP = os.environ.get("GT_WS_STOP", "")
TOPN = int(os.environ.get("GT_WS_TOPN", "40"))

import astroid
from astroid import transforms as atr
from astroid import context as actx

active = [FROM == ""]
counts = collections.Counter()
node_counts = collections.Counter()
total = [0]

orig_transform = atr.TransformVisitor._transform


def traced_transform(self, node):
    cls = node.__class__
    for transform_func, predicate in self.transforms[cls]:
        if predicate is None or predicate(node):
            ret = transform_func(node)
            if ret is not None:
                total[0] += 1
                if active[0]:
                    fn = getattr(transform_func, "__name__", repr(transform_func))
                    # try to find the brain module
                    mod = getattr(transform_func, "__module__", "?")
                    label = f"{mod}.{fn}"
                    # for inference_tip transforms, capture the call func name
                    if cls.__name__ == "Call":
                        f2 = getattr(node, "func", None)
                        fname = getattr(f2, "attrname", None) or getattr(f2, "name", None) or "?"
                        label = f"{label}::{fname}"
                    counts[label] += 1
                    node_counts[cls.__name__] += 1
                actx._invalidate_cache()
                node = ret
            if ret.__class__ != cls:
                break
    return node


atr.TransformVisitor._transform = traced_transform

# hook pylinter to know which file is being checked (phase 2)
import pylint.lint.pylinter as plinter
orig_check_astroid = plinter.PyLinter.check_astroid_module


def traced_check(self, ast_node, walker, rawcheckers, tokencheckers):
    fn = getattr(ast_node, "file", "") or ""
    rel = fn.replace(os.getcwd() + os.sep, "")
    if FROM and FROM in rel and not active[0]:
        active[0] = True
        sys.stderr.write(f"WS_START at {rel} total_wipes_so_far={total[0]}\n")
    base = total[0]
    r = orig_check_astroid(self, ast_node, walker, rawcheckers, tokencheckers)
    if active[0]:
        sys.stderr.write(f"WS_FILE {rel} +{total[0]-base} (total={total[0]})\n")
    if STOP and STOP in rel:
        sys.stderr.write(f"WS_STOP at {rel} total={total[0]}\n")
        sys.stderr.write("=== TOP transform sources ===\n")
        for k, v in counts.most_common(TOPN):
            sys.stderr.write(f"  {v:6d}  {k}\n")
        sys.stderr.write("=== by node class ===\n")
        for k, v in node_counts.most_common(20):
            sys.stderr.write(f"  {v:6d}  {k}\n")
        sys.stderr.flush()
        os._exit(0)
    return r


plinter.PyLinter.check_astroid_module = traced_check

from pylint import lint
flags = open(flagsfile).read().split()
flags = [f.replace("HARNESS_EMPTY_RC", rcfile) for f in flags]
try:
    lint.Run(["."] + flags, exit=False)
except SystemExit:
    pass
