#!/usr/bin/env python3
"""Globally (NOT gated on the W0231 check) trace every _T_co Name inference and
every TypeVar(...) Call inference reached through it, logging bn/cc/lk/ni and
whether the boundnode is set. The poison entry is MINTED during phase-2
properties.py processing (BEFORE the MappedColumn W0231 check, which only
replays it), so the W0231-gated tracers miss the deciding mint. This logs the
deciding cold mint: the FIRST inference of the Mapped[_T_co] base Subscript
(orm/base.py:844) at lk='__init__', and every _T_co Name/Call beneath it.

We arm a flag when a Subscript at base.py line 844 begins inferring at
lk='__init__', and within that window log every _T_co Name + every Call infer
with its bn, to count how many TypeVar Call re-runs astroid does at bn=set.

Usage: .venv-pylint/bin/python harness/trace_gt_tco_global.py <corpus> <flagsfile> 2> t.txt
"""
import sys, os
corpus = os.path.abspath(sys.argv[1]); flagsfile = os.path.abspath(sys.argv[2])
rcfile = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "empty.rcfile"))
os.chdir(corpus)

import astroid
from astroid import nodes, util

DEPTH = [0]
ARMED = [0]   # >0 while inside a lk='__init__' Mapped[_T_co] subscript infer
CALLS = [0]   # count of Call infers while ARMED
TCO = [0]     # count of _T_co Name infers while ARMED
MINTED = [False]

def fname(n):
    try:
        return os.path.basename(n.root().file or "")
    except Exception:
        return "?"

_orig_infer = nodes.NodeNG.infer
def traced_infer(self, context=None, **kw):
    nm = getattr(self, "name", None)
    bn = context.boundnode if context else None
    lk = context.lookupname if context else None
    ni = context.nodes_inferred if context else -1
    is_target_sub = (isinstance(self, nodes.Subscript)
                     and getattr(self, "lineno", 0) == 844
                     and fname(self) == "base.py")
    arm_here = False
    # Arm on the FIRST lk='__init__' infer of the base.py:844 subscript that is
    # in properties.py context (we just arm on lk='__init__' globally for the
    # 844 subscript; the deciding mint is the first one that goes over cap).
    if is_target_sub and lk == "__init__" and not MINTED[0]:
        ARMED[0] += 1
        arm_here = True
        sys.stderr.write(
            f"ARM Subscript@844 lk={lk!r} cc={context.callcontext is not None} "
            f"bn={getattr(bn,'name',type(bn).__name__) if bn is not None else 'None'} ni={ni}\n")
        sys.stderr.flush()
    if ARMED[0] > 0:
        if isinstance(self, nodes.Name) and nm == "_T_co":
            TCO[0] += 1
            bns = getattr(bn, "name", type(bn).__name__) if bn is not None else "None"
            sys.stderr.write(f"  TCO Name _T_co bn={bns} cc={context.callcontext is not None} lk={lk!r} ni={ni}\n")
        elif isinstance(self, nodes.Call):
            # is this a TypeVar(...) call?
            fn = ""
            try:
                fn = self.func.as_string()
            except Exception:
                pass
            if "TypeVar" in fn:
                CALLS[0] += 1
                bns = getattr(bn, "name", type(bn).__name__) if bn is not None else "None"
                sys.stderr.write(f"  CALL TypeVar bn={bns} cc={context.callcontext is not None} lk={lk!r} ni={ni}\n")
    try:
        yield from _orig_infer(self, context, **kw)
    finally:
        if arm_here:
            ARMED[0] -= 1
            if ARMED[0] == 0 and not MINTED[0]:
                MINTED[0] = True
                sys.stderr.write(f"MINT-DONE TypeVarCalls={CALLS[0]} TcoNames={TCO[0]} finalNi={context.nodes_inferred if context else -1}\n")
                sys.stderr.flush()
nodes.NodeNG.infer = traced_infer

from pylint import lint
flags = [f.replace("HARNESS_EMPTY_RC", rcfile) for f in open(flagsfile).read().split()]
try:
    lint.Run(["."] + flags, exit=False)
except SystemExit:
    pass
sys.stderr.write(f"GTDONE TypeVarCalls={CALLS[0]} TcoNames={TCO[0]}\n")
