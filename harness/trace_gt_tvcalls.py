#!/usr/bin/env python3
"""Count TypeVar(...) Call inferences done at bn=set (boundnode) vs bn=None
during the SINGLE cold _DeclarativeMapped.declared_metaclass(lk='__init__')
window — to compare against prylint's 6 redundant bn=true TypeVar Call re-runs.
We arm during that window via a generator-safe wrapper on declared_metaclass,
and tally Call._infer entries whose func renders as TypeVar.

Usage: .venv-pylint/bin/python harness/trace_gt_tvcalls.py <corpus> <flagsfile> 2> t.txt
"""
import sys, os
corpus = os.path.abspath(sys.argv[1]); flagsfile = os.path.abspath(sys.argv[2])
rcfile = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "empty.rcfile"))
os.chdir(corpus)

import astroid
from astroid import nodes, util

ARMED = [0]
DONE = [False]
EVENTS = []   # (kind, bn, cc, lk, ni)

_orig_dm = nodes.ClassDef.declared_metaclass
def traced_dm(self, context=None):
    lk = context.lookupname if context else None
    arm = (getattr(self, "name", "") == "_DeclarativeMapped" and lk == "__init__" and not DONE[0])
    if arm:
        ARMED[0] += 1
    try:
        return _orig_dm(self, context)
    finally:
        if arm:
            ARMED[0] -= 1
            if ARMED[0] == 0:
                DONE[0] = True
                tv = sum(1 for e in EVENTS if e[0] == "Call")
                tco = sum(1 for e in EVENTS if e[0] == "Tco")
                tv_bn = sum(1 for e in EVENTS if e[0] == "Call" and e[1] != "None")
                tco_bn = sum(1 for e in EVENTS if e[0] == "Tco" and e[1] != "None")
                sys.stderr.write(f"WINDOW-DONE TypeVarCalls={tv} (bn-set={tv_bn})  TcoNames={tco} (bn-set={tco_bn})  finalNi={context.nodes_inferred if context else -1}\n")
                for e in EVENTS:
                    sys.stderr.write(f"  EV {e[0]} bn={e[1]} cc={e[2]} lk={e[3]!r} ni={e[4]}\n")
                sys.stderr.flush()
nodes.ClassDef.declared_metaclass = traced_dm

_orig_infer = nodes.NodeNG.infer
def traced_infer(self, context=None, **kw):
    if ARMED[0] > 0 and not DONE[0]:
        bn = context.boundnode if context else None
        bns = getattr(bn, "name", type(bn).__name__) if bn is not None else "None"
        cc = (context.callcontext is not None) if context else False
        lk = context.lookupname if context else None
        ni = context.nodes_inferred if context else -1
        nm = getattr(self, "name", None)
        if isinstance(self, nodes.Name) and nm == "_T_co":
            EVENTS.append(("Tco", bns, cc, lk, ni))
        elif isinstance(self, nodes.Call):
            try:
                fn = self.func.as_string()
            except Exception:
                fn = ""
            if "TypeVar" in fn:
                EVENTS.append(("Call", bns, cc, lk, ni))
    yield from _orig_infer(self, context, **kw)
nodes.NodeNG.infer = traced_infer

from pylint import lint
flags = [f.replace("HARNESS_EMPTY_RC", rcfile) for f in open(flagsfile).read().split()]
try:
    lint.Run(["."] + flags, exit=False)
except SystemExit:
    pass
sys.stderr.write("GTDONE\n")
