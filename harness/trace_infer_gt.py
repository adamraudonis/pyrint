import sys, os
sys.path.insert(0, os.path.expanduser('~/Desktop/Projects/prylint/harness'))
import dump_infer
import astroid
from astroid.nodes.node_ng import NodeNG

depth = [0]
orig = NodeNG.infer
def traced(self, context=None, **kwargs):
    name = getattr(self, 'name', None) or getattr(self, 'attrname', None) or ''
    ln = context.lookupname if context else None
    sys.stderr.write('  ' * depth[0] + f'> {type(self).__name__} {name} ln={ln!r}\n')
    depth[0] += 1
    try:
        for v in orig(self, context=context, **kwargs):
            depth[0] -= 1
            sys.stderr.write('  ' * depth[0] + f'yield {dump_infer.render(v)}\n')
            depth[0] += 1
            yield v
    finally:
        depth[0] -= 1
NodeNG.infer = traced
dump_infer.main()
