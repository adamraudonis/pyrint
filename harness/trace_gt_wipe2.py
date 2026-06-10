import sys, os
sys.path.insert(0, os.path.expanduser('~/Desktop/Projects/prylint/harness'))
import dump_infer
import astroid
from astroid import context as actx
from astroid.nodes.node_ng import NodeNG

depth = [0]
orig = NodeNG.infer
def traced(self, context=None, **kwargs):
    name = getattr(self, 'name', None) or getattr(self, 'attrname', None) or ''
    ln = context.lookupname if context else None
    ni = context.nodes_inferred if context else -1
    key_in = 'HIT' if False else 'MISS'
    sys.stderr.write('  ' * depth[0] + f'> {type(self).__name__} {name} ln={ln!r} ni={ni}\n')
    depth[0] += 1
    try:
        for v in orig(self, context=context, **kwargs):
            depth[0] -= 1
            sys.stderr.write('  ' * depth[0] + f'yield {dump_infer.render(v)} ni={context.nodes_inferred if context else -1}\n')
            depth[0] += 1
            yield v
    finally:
        depth[0] -= 1
NodeNG.infer = traced

orig_inval = actx._invalidate_cache
def traced_inval():
    sys.stderr.write('WIPE\n')
    return orig_inval()
actx._invalidate_cache = traced_inval
# transforms.py imported _invalidate_cache by value — patch there too
from astroid import transforms as atr
atr._invalidate_cache = traced_inval

dump_infer.main()
