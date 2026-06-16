#!/usr/bin/env python3
"""During the MappedColumn W0231 igetattr, log the InferenceContext bn/cc/lk
for EVERY _T_co Name inference (and the Mapped[_T_co] Subscript), to confirm
whether astroid reaches the _T_co TypeVar tip at bn=None (cache-hit replay) or
bn=set (re-run). Patches NodeNG.infer to log when self is a _T_co Name in
orm/base.py reached while a MappedColumn igetattr is on the stack.

Usage: .venv-pylint/bin/python harness/trace_gt_tco.py <corpus> <flagsfile> 2> t.txt
"""
import sys, os
corpus = os.path.abspath(sys.argv[1]); flagsfile = os.path.abspath(sys.argv[2])
rcfile = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "empty.rcfile"))
os.chdir(corpus)

import astroid
from astroid import nodes, util
import pylint.checkers.classes.class_checker as cc

ACTIVE = [False]
_orig_atc = cc._ancestors_to_call
def traced_atc(klass_node, method_name="__init__"):
    if getattr(klass_node, "name", "") == "MappedColumn":
        ACTIVE[0] = True
        try:
            return _orig_atc(klass_node, method_name)
        finally:
            ACTIVE[0] = False
    return _orig_atc(klass_node, method_name)
cc._ancestors_to_call = traced_atc

_orig_infer = nodes.NodeNG.infer
def traced_infer(self, context=None, **kw):
    if ACTIVE[0]:
        nm = getattr(self, "name", None)
        try:
            fn = os.path.basename(self.root().file or "")
        except Exception:
            fn = "?"
        if (isinstance(self, nodes.Name) and nm == "_T_co") or \
           (isinstance(self, nodes.Subscript) and getattr(self, "lineno", 0) == 844):
            bn = context.boundnode if context else None
            ccx = context.callcontext if context else None
            lk = context.lookupname if context else None
            ni = context.nodes_inferred if context else -1
            bns = type(bn).__name__ if bn is not None else "None"
            sys.stderr.write(
                f"TCO {type(self).__name__} {nm or self.as_string()[:20]} @{getattr(self,'lineno','?')} "
                f"lk={lk!r} cc={ccx is not None} bn={bns} ni={ni}\n")
            sys.stderr.flush()
    return _orig_infer(self, context, **kw)
nodes.NodeNG.infer = traced_infer

from pylint import lint
flags = [f.replace("HARNESS_EMPTY_RC", rcfile) for f in open(flagsfile).read().split()]
try:
    lint.Run(["."] + flags, exit=False)
except SystemExit:
    pass
sys.stderr.write("GTDONE\n")
