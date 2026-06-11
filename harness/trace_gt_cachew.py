#!/usr/bin/env python3
"""trace_gt_file.py variant that ALSO logs global inference-cache writes
(CACHEW lines) by wrapping astroid.context._INFERENCE_CACHE in a logging
dict. Pairs with the rust side's CACHEW lines under PRYLINT_TRACE_INFER.
Env: GT_TRACE_FILE=<path substring> gates output like trace_gt_file.py.
"""
import sys, os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import dump_infer
import astroid
from astroid import context as actx
from astroid.nodes.node_ng import NodeNG

TRACE_SUB = os.environ.get('GT_TRACE_FILE', '')
enabled = [TRACE_SUB == '']
depth = [0]
orig = NodeNG.infer


class LogDict(dict):
    def __setitem__(self, key, value):
        if enabled[0]:
            node = key[0]
            nm = getattr(node, 'name', None) or getattr(node, 'attrname', None) or ''
            sys.stderr.write(f'CACHEW {type(node).__name__} {nm} vals={len(value)}\n')
        super().__setitem__(key, value)


actx._INFERENCE_CACHE = LogDict(actx._INFERENCE_CACHE)


def traced(self, context=None, **kwargs):
    if not enabled[0]:
        yield from orig(self, context=context, **kwargs)
        return
    name = getattr(self, 'name', None) or getattr(self, 'attrname', None) or ''
    ln = context.lookupname if context else None
    ni = context.nodes_inferred if context else -1
    cc = id(context.callcontext) if context and context.callcontext else None
    bn = (type(context.boundnode).__name__) if context and context.boundnode else None
    sys.stderr.write('  ' * depth[0] + f'> {type(self).__name__} {name} ln={ln!r} cc={cc} bn={bn} ni={ni}\n')
    depth[0] += 1
    try:
        for v in orig(self, context=context, **kwargs):
            depth[0] -= 1
            sys.stderr.write('  ' * depth[0] + f'yield {dump_infer.render(v)} ni={context.nodes_inferred if context else -1}\n')
            depth[0] += 1
            yield v
    finally:
        depth[0] -= 1


orig_inval = actx._invalidate_cache
def traced_inval():
    if enabled[0]:
        sys.stderr.write('WIPE\n')
    return orig_inval()
actx._invalidate_cache = traced_inval
from astroid import transforms as atr
atr._invalidate_cache = traced_inval

_orig_infer_node = dump_infer.infer_node
def marked(n):
    if not enabled[0]:
        try:
            f = n.root().file or ''
        except Exception:
            f = ''
        if TRACE_SUB and TRACE_SUB in f:
            enabled[0] = True
    if enabled[0]:
        sys.stderr.write(f'@@DUMPNODE {n.fromlineno}:{n.col_offset}:{type(n).__name__}\n')
    depth[0] = 0
    return _orig_infer_node(n)

dump_infer.infer_node = marked
NodeNG.infer = traced
del sys.modules["dump_infer"]
dump_infer.main()
