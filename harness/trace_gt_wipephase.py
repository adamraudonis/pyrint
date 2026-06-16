#!/usr/bin/env python3
"""Report astroid global-cache wipe count at phase boundaries.

Patches astroid.context._invalidate_cache (and transforms._invalidate_cache)
to count wipes, and pylint's _check_astroid_module to log the wipe count at
the START of each module's check (phase 2). If the wipe count keeps growing
during phase 2, astroid wipes mid-check like prylint; if it's flat, it doesn't.

Usage: .venv-pylint/bin/python harness/trace_gt_wipephase.py <corpus> <flagsfile> 2> t.txt
"""
import sys, os
corpus = os.path.abspath(sys.argv[1]); flagsfile = os.path.abspath(sys.argv[2])
rcfile = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "empty.rcfile"))
os.chdir(corpus)

import astroid
from astroid import context as actx, transforms as atr

_wipe = [0]
_oi = actx._invalidate_cache
def _ti():
    _wipe[0] += 1
    return _oi()
actx._invalidate_cache = _ti
atr._invalidate_cache = _ti

import pylint.lint.pylinter as plinter
_orig = plinter.PyLinter._check_astroid_module
_count = [0]
def traced(self, node, walker, rawcheckers, tokencheckers):
    _count[0] += 1
    fn = os.path.basename(getattr(node, "file", "") or "")
    if _count[0] <= 3 or "properties" in fn or "attributes" in fn or "base.py" in fn:
        sys.stderr.write(f"PHASE2 check#{_count[0]} {fn} wipes={_wipe[0]}\n")
    return _orig(self, node, walker, rawcheckers, tokencheckers)
plinter.PyLinter._check_astroid_module = traced

from pylint import lint
flags = [f.replace("HARNESS_EMPTY_RC", rcfile) for f in open(flagsfile).read().split()]
try:
    lint.Run(["."] + flags, exit=False)
except SystemExit:
    pass
sys.stderr.write(f"GTDONE total_wipes={_wipe[0]}\n")
