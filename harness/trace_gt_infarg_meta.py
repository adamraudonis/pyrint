#!/usr/bin/env python3
"""During the MappedColumn W0231 igetattr, log every CallSite.infer_argument
call that reaches the `method_scope is boundnode.metaclass(context=context)`
branch (arguments.py:230), reporting context.boundnode at that point — to
confirm whether astroid passes bn=None (vs prylint's bn=set) into the
metaclass walk that drives the _T_co re-inferences.

Usage: .venv-pylint/bin/python harness/trace_gt_infarg_meta.py <corpus> <flagsfile> 2> t.txt
"""
import sys, os
corpus = os.path.abspath(sys.argv[1]); flagsfile = os.path.abspath(sys.argv[2])
rcfile = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "empty.rcfile"))
os.chdir(corpus)

import astroid
from astroid import nodes
from astroid.arguments import CallSite
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

# Patch ClassDef.metaclass to log context.boundnode when called while ACTIVE
_orig_meta = nodes.ClassDef.metaclass
def traced_meta(self, context=None):
    if ACTIVE[0] and getattr(self, "name", "") in {
        "SQLORMExpression", "Mapped", "_DeclarativeMapped", "_MappedAttribute",
        "SQLCoreOperations", "SQLColumnExpression",
    }:
        bn = context.boundnode if context else None
        bns = getattr(bn, "name", type(bn).__name__) if bn is not None else "None"
        ni = context.nodes_inferred if context else -1
        sys.stderr.write(f"META cls={self.name} bn={bns} ni={ni}\n")
        sys.stderr.flush()
    return _orig_meta(self, context)
nodes.ClassDef.metaclass = traced_meta

from pylint import lint
flags = [f.replace("HARNESS_EMPTY_RC", rcfile) for f in open(flagsfile).read().split()]
try:
    lint.Run(["."] + flags, exit=False)
except SystemExit:
    pass
sys.stderr.write("GTDONE\n")
