#!/usr/bin/env python3
"""Trace astroid's LINT-PATH resolution at the sqlalchemy FP class sites.

Monkeypatches ClassDef.ancestors so that, for the specific FP target classes
(by file:line), it logs every base ClassDef yielded + the current wipe count.
Also patches ClassDef.local_attr / igetattr for __init__ to see whether the
base's __init__ resolves. This shows, in the real full-corpus warm run, whether
astroid yields the problematic generic base (-> W0231/R0901/W0223 FP) or skips
it (cold-truncated -> no FP).

Usage: .venv-pylint/bin/python harness/trace_gt_sqla.py <corpus_dir> <flagsfile>
2> trace.txt
"""
import sys, os

corpus = os.path.abspath(sys.argv[1])
flagsfile = os.path.abspath(sys.argv[2])
rcfile = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "empty.rcfile"))
os.chdir(corpus)

import astroid
from astroid import nodes, bases, util
from astroid import context as _actx
from astroid import transforms as _atr

# wipe counter
_wipe_count = [0]
_orig_inval = _actx._invalidate_cache
def _traced_inval():
    _wipe_count[0] += 1
    return _orig_inval()
_actx._invalidate_cache = _traced_inval
_atr._invalidate_cache = _traced_inval

# Target FP class sites: (basename, lineno)
TARGETS = {
    ("properties.py", 555),   # MappedSQLExpression / Column owner of __init__@565? trace whole class
    ("properties.py", 561),
    ("attributes.py", 198),
    ("attributes.py", 620),
    ("array.py", 93),
}

def _matches(node):
    try:
        root = node.root()
        fn = getattr(root, "file", "") or ""
        base = os.path.basename(fn)
    except Exception:
        return None
    ln = getattr(node, "fromlineno", None)
    for (b, l) in TARGETS:
        if base == b:
            return (base, ln)
    return None

FPCLASSES = {"array", "MappedColumn", "QueryableAttribute", "Proxy"}
_seen = {}
_orig_ancestors = nodes.ClassDef.ancestors
def _traced_ancestors(self, recurs=True, context=None):
    cname = self.name
    if cname not in FPCLASSES or recurs is not False:
        yield from _orig_ancestors(self, recurs=recurs, context=context)
        return
    try:
        fn = os.path.basename(getattr(self.root(), "file", "") or "")
    except Exception:
        fn = "?"
    key = (cname, fn, getattr(self, "fromlineno", 0))
    _seen[key] = _seen.get(key, 0) + 1
    cnt = _seen[key]
    bases = []
    n = 0
    for a in _orig_ancestors(self, recurs=False, context=context):
        n += 1
        bases.append(getattr(a, "name", "?"))
        yield a
    # only log the FIRST few times per class to bound output
    if cnt <= 4:
        sys.stderr.write(f"GTANC #{cnt} class={cname} {fn}:{getattr(self,'fromlineno',0)} nbases={n} bases={bases} wipes={_wipe_count[0]}\n")
        sys.stderr.flush()
nodes.ClassDef.ancestors = _traced_ancestors

# run pylint
from pylint import lint
flags = open(flagsfile).read().split()
flags = [f.replace("HARNESS_EMPTY_RC", rcfile) for f in flags]
try:
    lint.Run(["."] + flags, exit=False)
except SystemExit:
    pass
sys.stderr.write(f"GTDONE total_wipes={_wipe_count[0]}\n")
