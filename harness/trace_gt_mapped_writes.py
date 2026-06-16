#!/usr/bin/env python3
"""Trace every _INFERENCE_CACHE write whose key node is the `Mapped[_T_co]`
Subscript base of _DeclarativeMapped (orm/base.py line 844), reporting the
lookupname/cc/bn + result kinds + nodes_inferred-at-write, to see whether
astroid EVER writes a `(Mapped[_T_co], '__init__', None, None) = (U,)` poison.

Wraps _INFERENCE_CACHE in a dict subclass that logs matching __setitem__.

Usage: .venv-pylint/bin/python harness/trace_gt_mapped_writes.py <corpus> <flagsfile> 2> t.txt
"""
import sys, os
corpus = os.path.abspath(sys.argv[1]); flagsfile = os.path.abspath(sys.argv[2])
rcfile = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "empty.rcfile"))
os.chdir(corpus)

import astroid
from astroid import nodes, util, context as actx

TARGET_LINE = 844  # _DeclarativeMapped class def line in orm/base.py

def desc(t):
    out = []
    for r in t:
        if isinstance(r, util.UninferableBase):
            out.append("U")
        elif isinstance(r, nodes.ClassDef):
            out.append("Class:" + r.name)
        else:
            out.append(type(r).__name__)
    return out

class LoggingCache(dict):
    def __setitem__(self, key, value):
        node = key[0]
        try:
            if (isinstance(node, nodes.Subscript)
                    and getattr(node, "lineno", None) == TARGET_LINE
                    and "base.py" in (node.root().file or "")
                    and node.col_offset == 25):
                lk = key[1]
                cc = key[2] is not None
                bn = key[3] is not None
                sys.stderr.write(
                    f"MAPWRITE lk={lk!r} cc={cc} bn={bn} -> {desc(value)}\n"
                )
                sys.stderr.flush()
        except Exception:
            pass
        super().__setitem__(key, value)

# swap the module-global cache with our logging one, and rebind the property
newcache = LoggingCache()
actx._INFERENCE_CACHE = newcache
# the property reads the module global each access, so this is enough, but
# _invalidate_cache does _INFERENCE_CACHE.clear() on the ORIGINAL name — patch it
_orig_inval = actx._invalidate_cache
def _inval():
    newcache.clear()
actx._invalidate_cache = _inval
import astroid.transforms as atr
atr._invalidate_cache = _inval

from pylint import lint
flags = [f.replace("HARNESS_EMPTY_RC", rcfile) for f in open(flagsfile).read().split()]
try:
    lint.Run(["."] + flags, exit=False)
except SystemExit:
    pass
sys.stderr.write("GTDONE\n")
