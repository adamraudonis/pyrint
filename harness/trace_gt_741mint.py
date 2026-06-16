#!/usr/bin/env python3
"""Globally log the FIRST cold mint (cache MISS) of the base.py:741:4
SQLORMOperations[_T_co] Subscript at bn=SQLORMExpression (the entry astroid HITS
but prylint MISSES at the deciding W0231 mint). Reports which FILE astroid is
linting when it first warms that bn=set entry, to confirm cross-file warmth vs
in-resolution warmth.

Usage: .venv-pylint/bin/python harness/trace_gt_741mint.py <corpus> <flagsfile> 2> t.txt
"""
import sys, os
corpus = os.path.abspath(sys.argv[1]); flagsfile = os.path.abspath(sys.argv[2])
rcfile = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "empty.rcfile"))
os.chdir(corpus)

import astroid
from astroid import nodes, util
from astroid.context import _INFERENCE_CACHE

CURFILE = ["?"]
import pylint.lint.pylinter as plmod
_orig_check_astroid = None
# track current file via linter's _check_astroid_module path is hard; instead
# track via PyLinter.check_single_file_item if present
try:
    import pylint.lint.pylinter as P
    _orig = P.PyLinter._check_files
except Exception:
    _orig = None

# Simpler: hook get_ast / file_state. Use the module being linted via
# linter.current_file through a check-start callback is unavailable; approximate
# by recording the root file of the Subscript's *enclosing* infer call stack is
# also hard. We instead just log a global counter + whether it is the FIRST miss.

N = [0]
FIRST_DONE = [False]
_orig_infer = nodes.NodeNG.infer
def traced_infer(self, context=None, **kw):
    if (isinstance(self, nodes.Subscript) and getattr(self, "lineno", 0) == 741
            and getattr(self, "col_offset", -1) == 4
            and os.path.basename(self.root().file or "") == "base.py"
            and context is not None and context.boundnode is not None
            and not FIRST_DONE[0]):
        key = (self, context.lookupname, context.callcontext, context.boundnode)
        hit = key in _INFERENCE_CACHE
        bn = context.boundnode
        bns = getattr(bn, "name", type(bn).__name__)
        if not hit:
            N[0] += 1
            sys.stderr.write(f"741MINT #{N[0]} (COLD MISS) bn={bns} lk={context.lookupname!r} ni={context.nodes_inferred} curfile={_curfile()}\n")
            sys.stderr.flush()
            if N[0] >= 1:
                FIRST_DONE[0] = True
    yield from _orig_infer(self, context, **kw)
nodes.NodeNG.infer = traced_infer

# Track current linted file via PyLinter.set_current_module
_CUR = ["?"]
def _curfile():
    return _CUR[0]
import pylint.lint.pylinter as P
_orig_scm = P.PyLinter.set_current_module
def traced_scm(self, modname, filepath=None):
    _CUR[0] = os.path.basename(filepath or modname or "?")
    return _orig_scm(self, modname, filepath)
P.PyLinter.set_current_module = traced_scm

from pylint import lint
flags = [f.replace("HARNESS_EMPTY_RC", rcfile) for f in open(flagsfile).read().split()]
try:
    lint.Run(["."] + flags, exit=False)
except SystemExit:
    pass
sys.stderr.write(f"GTDONE first741mint_done={FIRST_DONE[0]}\n")
