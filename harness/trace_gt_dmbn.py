#!/usr/bin/env python3
"""During the cold _DeclarativeMapped.declared_metaclass(lk='__init__') window,
log the boundnode that astroid's declared_metaclass sees at ENTRY for EVERY
class it is called on (Mapped, SQLORMExpression, SQLCoreOperations, ...). This
pinpoints whether astroid's metaclass walk reaches these classes at bn=set
(like prylint) or bn=None. Generator-safe (does not consume).

Usage: .venv-pylint/bin/python harness/trace_gt_dmbn.py <corpus> <flagsfile> 2> t.txt
"""
import sys, os
corpus = os.path.abspath(sys.argv[1]); flagsfile = os.path.abspath(sys.argv[2])
rcfile = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "empty.rcfile"))
os.chdir(corpus)

import astroid
from astroid import nodes, util

ARMED = [0]
DONE = [False]
LOG = []

_orig_dm = nodes.ClassDef.declared_metaclass
def traced_dm(self, context=None):
    lk = context.lookupname if context else None
    bn = context.boundnode if context else None
    bns = getattr(bn, "name", type(bn).__name__) if bn is not None else "None"
    cc = (context.callcontext is not None) if context else False
    arm = (getattr(self, "name", "") == "_DeclarativeMapped" and lk == "__init__" and not DONE[0])
    if ARMED[0] > 0 and not DONE[0]:
        LOG.append(f"  DM cls={self.name} bn={bns} cc={cc} lk={lk!r} ni={context.nodes_inferred if context else -1}")
    if arm:
        ARMED[0] += 1
    try:
        return _orig_dm(self, context)
    finally:
        if arm:
            ARMED[0] -= 1
            if ARMED[0] == 0:
                DONE[0] = True
                sys.stderr.write("DMBN-WINDOW:\n" + "\n".join(LOG) + "\n")
                sys.stderr.flush()
nodes.ClassDef.declared_metaclass = traced_dm

# also log metaclass() boundnode while armed
_orig_meta = nodes.ClassDef.metaclass
def traced_meta(self, context=None):
    if ARMED[0] > 0 and not DONE[0]:
        bn = context.boundnode if context else None
        bns = getattr(bn, "name", type(bn).__name__) if bn is not None else "None"
        LOG.append(f"  META cls={self.name} bn={bns}")
    return _orig_meta(self, context)
nodes.ClassDef.metaclass = traced_meta

from pylint import lint
flags = [f.replace("HARNESS_EMPTY_RC", rcfile) for f in open(flagsfile).read().split()]
try:
    lint.Run(["."] + flags, exit=False)
except SystemExit:
    pass
sys.stderr.write("GTDONE\n")
