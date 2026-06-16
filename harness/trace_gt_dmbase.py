#!/usr/bin/env python3
"""Trace astroid's declared_metaclass for _DeclarativeMapped: log the
context.lookupname seen at entry, and whether it infers its bases (base.py:844)
and at what lk. Confirms whether astroid's metaclass walk reaches the
Mapped[_T_co] base subscript at lk='__init__' (matching prylint) or lk=None.

Usage: .venv-pylint/bin/python harness/trace_gt_dmbase.py <corpus> <flagsfile> 2> t.txt
"""
import sys, os
corpus = os.path.abspath(sys.argv[1]); flagsfile = os.path.abspath(sys.argv[2])
rcfile = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "empty.rcfile"))
os.chdir(corpus)

import astroid
from astroid import nodes, util

from collections import Counter
DMLK = Counter()
_orig_dm = nodes.ClassDef.declared_metaclass
def traced_dm(self, context=None):
    if getattr(self, "name", "") == "_DeclarativeMapped":
        lk = context.lookupname if context else None
        DMLK[("DM", lk)] += 1
        if lk == "__init__":
            sys.stderr.write(f"DM-INIT _DeclarativeMapped lk={lk!r} ni={context.nodes_inferred if context else -1}\n")
            sys.stderr.flush()
    return _orig_dm(self, context)
nodes.ClassDef.declared_metaclass = traced_dm

_orig_meta = nodes.ClassDef.metaclass
def traced_meta(self, context=None):
    if getattr(self, "name", "") == "_DeclarativeMapped":
        lk = context.lookupname if context else None
        DMLK[("META", lk)] += 1
        if lk == "__init__":
            sys.stderr.write(f"META-INIT _DeclarativeMapped lk={lk!r}\n")
            sys.stderr.flush()
    return _orig_meta(self, context)
nodes.ClassDef.metaclass = traced_meta

import atexit
atexit.register(lambda: sys.stderr.write(f"DMLK-SUMMARY {dict(DMLK)}\n"))

from pylint import lint
flags = [f.replace("HARNESS_EMPTY_RC", rcfile) for f in open(flagsfile).read().split()]
try:
    lint.Run(["."] + flags, exit=False)
except SystemExit:
    pass
sys.stderr.write("GTDONE\n")
