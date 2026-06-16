#!/usr/bin/env python3
"""GT cache-key logger for the _DeclarativeMapped.declared_metaclass FP chain.

Gated to the W0231 _ancestors_to_call of MappedColumn/QueryableAttribute/array:
wraps ClassDef.declared_metaclass so that ONLY during the deciding
_DeclarativeMapped.declared_metaclass(lookupname='__init__') call do we log,
for every NodeNG.infer entry, the exact cache key components
(node-repr, lookupname, callcontext-is-None, boundnode-is-None) + nodes_inferred
at entry. This shows precisely how astroid keys the _T_co / subscript-index
inferences reached through the bn=true metaclass walk.

Usage: .venv-pylint/bin/python harness/trace_gt_dmkey.py <corpus> <flagsfile> 2> t.txt
"""
import sys, os
corpus = os.path.abspath(sys.argv[1]); flagsfile = os.path.abspath(sys.argv[2])
rcfile = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "empty.rcfile"))
os.chdir(corpus)

import astroid
from astroid import nodes, bases, util
from astroid.nodes.node_ng import NodeNG

ACTIVE = [False]
DEPTH = [0]

def nrepr(n):
    try:
        cn = type(n).__name__
        nm = getattr(n, "name", None) or getattr(n, "attrname", None) or ""
        ln = getattr(n, "lineno", "?")
        return f"{cn}:{nm}@{ln}"
    except Exception:
        return "?"

_orig_infer = NodeNG.infer
def traced_infer(self, context=None, **kw):
    if not ACTIVE[0]:
        yield from _orig_infer(self, context, **kw)
        return
    lk = getattr(context, "lookupname", None) if context else None
    cc = getattr(context, "callcontext", None) if context else None
    bn = getattr(context, "boundnode", None) if context else None
    ni = getattr(context, "nodes_inferred", -1) if context else -1
    ind = "  " * DEPTH[0]
    nm = getattr(self, "name", None) or getattr(self, "attrname", None) or ""
    # only log the interesting nodes to keep output small
    log_it = ("_T_co" in str(nm)) or isinstance(self, (nodes.Subscript,)) or ("Mapped" in str(nm)) or ("_MappedAttribute" in str(nm))
    if log_it:
        if bn is None:
            bnr = "-"
        else:
            try:
                bnr = f"{type(bn).__name__}:{getattr(bn,'name',None) or getattr(getattr(bn,'_proxied',None),'name',None) or ''}@{getattr(bn,'lineno','?')}/{id(bn)%100000}"
            except Exception:
                bnr = "?"
        ccr = "-"
        if cc is not None:
            ccr = f"cc{id(cc)%100000}"
        sys.stderr.write(f"{ind}IN {nrepr(self)} lk={lk!r} cc={ccr} bn={bnr} ni={ni}\n")
    DEPTH[0] += 1
    cnt = 0
    for r in _orig_infer(self, context, **kw):
        cnt += 1
        yield r
    DEPTH[0] -= 1
    if log_it:
        ni2 = getattr(context, "nodes_inferred", -1) if context else -1
        sys.stderr.write(f"{ind}OUT {nrepr(self)} n={cnt} ni->{ni2}\n")
NodeNG.infer = traced_infer

_orig_meta = nodes.ClassDef.metaclass
def traced_meta(self, context=None):
    if ACTIVE[0]:
        bn = getattr(context, "boundnode", None) if context else None
        bnr = "-" if bn is None else f"{type(bn).__name__}:{getattr(bn,'name','')}"
        ni0 = getattr(context, "nodes_inferred", -1) if context else -1
        sys.stderr.write(f"{'  '*DEPTH[0]}META({getattr(self,'name','?')}) bn={bnr} ni={ni0}\n")
        r = _orig_meta(self, context)
        ni1 = getattr(context, "nodes_inferred", -1) if context else -1
        sys.stderr.write(f"{'  '*DEPTH[0]}META({getattr(self,'name','?')}) -> {type(r).__name__ if r is not None else 'None'} cost={ni1-ni0}\n")
        return r
    return _orig_meta(self, context)
nodes.ClassDef.metaclass = traced_meta

_orig_dm = nodes.ClassDef.declared_metaclass
def traced_dm(self, context=None):
    if getattr(self, "name", "") == "_DeclarativeMapped":
        lk = getattr(context, "lookupname", None) if context else None
        if lk == "__init__" and not ACTIVE[0]:
            sys.stderr.write(f"==== ENTER declared_metaclass _DeclarativeMapped lk={lk!r} cc={'X' if (context and context.callcontext is not None) else '-'} bn={'X' if (context and context.boundnode is not None) else '-'} ni={getattr(context,'nodes_inferred',-1) if context else -1}\n")
            ACTIVE[0] = True
            try:
                return _orig_dm(self, context)
            finally:
                ACTIVE[0] = False
                sys.stderr.write(f"==== EXIT declared_metaclass _DeclarativeMapped ni={getattr(context,'nodes_inferred',-1) if context else -1}\n")
    return _orig_dm(self, context)
nodes.ClassDef.declared_metaclass = traced_dm

from pylint import lint
flags = [f.replace("HARNESS_EMPTY_RC", rcfile) for f in open(flagsfile).read().split()]
try:
    lint.Run(["."] + flags, exit=False)
except SystemExit:
    pass
sys.stderr.write("GTDONE\n")
