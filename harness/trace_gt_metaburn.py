#!/usr/bin/env python3
"""Trace astroid's ctx.nodes_inferred across metaclass() then getattr() inside
ClassDef.igetattr for _DeclarativeMapped.__init__, in the warm full run.

Usage: .venv-pylint/bin/python harness/trace_gt_metaburn.py <corpus> <flagsfile> 2> t.txt
"""
import sys, os
corpus = os.path.abspath(sys.argv[1]); flagsfile = os.path.abspath(sys.argv[2])
rcfile = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "empty.rcfile"))
os.chdir(corpus)

import astroid
from astroid import nodes, bases, util, context as actx
from astroid.context import copy_context

_orig_igetattr = nodes.ClassDef.igetattr
def traced_igetattr(self, name, context=None, class_context=True):
    if self.name == "_DeclarativeMapped" and name == "__init__":
        ctx = copy_context(context)
        ctx.lookupname = name
        ni0 = ctx.nodes_inferred
        mc = self.metaclass(context=ctx)
        ni1 = ctx.nodes_inferred
        try:
            attrs = self.getattr(name, ctx, class_context=class_context)
            ni2 = ctx.nodes_inferred
            kinds = []
            for a in attrs:
                fr = getattr(getattr(a, 'parent', None), 'name', '?')
                kinds.append(f"{type(a).__name__}({fr})")
            sys.stderr.write(f"GTMETABURN ni_in={ni0} after_meta={ni1} after_getattr={ni2} nattrs={len(attrs)} kinds={kinds}\n")
        except Exception as e:
            ni2 = ctx.nodes_inferred
            sys.stderr.write(f"GTMETABURN ni_in={ni0} after_meta={ni1} after_getattr={ni2} EXC={type(e).__name__}\n")
        sys.stderr.flush()
    yield from _orig_igetattr(self, name, context=context, class_context=class_context)
nodes.ClassDef.igetattr = traced_igetattr

from pylint import lint
flags = [f.replace("HARNESS_EMPTY_RC", rcfile) for f in open(flagsfile).read().split()]
try:
    lint.Run(["."] + flags, exit=False)
except SystemExit:
    pass
