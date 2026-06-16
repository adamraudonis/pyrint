#!/usr/bin/env python3
"""Lifecycle of the base.py:741:4 SQLORMOperations[_T_co] subscript entry at
bn=SQLORMExpression (lk=None, cc=None) on the astroid side, mirroring prylint's
PRYLINT_DBG741 probe. Logs every WRITE (with current lint file + wipe count)
and every WIPE that removes a present entry. This reveals astroid's LAST warm
write of the deciding entry before properties.py (vs prylint, where it lands
during phase-1 build and is wiped before the properties.py check).

Usage: .venv-pylint/bin/python harness/trace_gt_741life.py <corpus> <flagsfile> 2> t.txt
"""
import sys, os
corpus = os.path.abspath(sys.argv[1]); flagsfile = os.path.abspath(sys.argv[2])
rcfile = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "empty.rcfile"))
os.chdir(corpus)

import astroid
from astroid import context as actx, nodes

WIPES = [0]
CUR = ["?"]

def _is_target(key):
    node = key[0]
    if not isinstance(node, nodes.Subscript):
        return False
    if getattr(node, "lineno", 0) != 741 or getattr(node, "col_offset", -1) != 4:
        return False
    try:
        if os.path.basename(node.root().file or "") != "base.py":
            return False
    except Exception:
        return False
    lk, cc, bn = key[1], key[2], key[3]
    if lk is not None or cc is not None:
        return False
    return getattr(bn, "name", None) == "SQLORMExpression"

PRESENT = [False]  # is the target entry currently in the cache?

class LogDict(dict):
    def __setitem__(self, key, value):
        if _is_target(key):
            PRESENT[0] = True
            sys.stderr.write(f"DBG741 WRITE file={CUR[0]} wipes={WIPES[0]}\n")
            sys.stderr.flush()
        super().__setitem__(key, value)
    def clear(self):
        WIPES[0] += 1
        if PRESENT[0]:
            PRESENT[0] = False
            sys.stderr.write(f"DBG741 WIPE(removes-target) file={CUR[0]} wipes={WIPES[0]}\n")
            sys.stderr.flush()
        super().clear()

actx._INFERENCE_CACHE = LogDict(actx._INFERENCE_CACHE)

# log when _DeclarativeMapped.declared_metaclass(lk='__init__') window opens
_orig_dm = nodes.ClassDef.declared_metaclass
def traced_dm(self, context=None):
    lk = context.lookupname if context else None
    if lk == "__init__" and "properties.py" in str(CUR[0]):
        sys.stderr.write(f"DMWALL cls={getattr(self,'name','')} wipes={WIPES[0]} ni={context.nodes_inferred if context else -1}\n")
        sys.stderr.flush()
    if getattr(self, "name", "") == "_DeclarativeMapped" and lk == "__init__":
        sys.stderr.write(f"DMW-OPEN file={CUR[0]} wipes={WIPES[0]} ni={context.nodes_inferred if context else -1}\n")
        sys.stderr.flush()
    return _orig_dm(self, context)
nodes.ClassDef.declared_metaclass = traced_dm

# track current linted file
import pylint.lint.pylinter as P
_orig_scm = P.PyLinter.set_current_module
def traced_scm(self, modname, filepath=None):
    CUR[0] = (filepath or modname or "?")
    return _orig_scm(self, modname, filepath)
P.PyLinter.set_current_module = traced_scm

from pylint import lint
flags = [f.replace("HARNESS_EMPTY_RC", rcfile) for f in open(flagsfile).read().split()]
try:
    lint.Run(["."] + flags, exit=False)
except SystemExit:
    pass
sys.stderr.write(f"GTDONE total_wipes={WIPES[0]}\n")
