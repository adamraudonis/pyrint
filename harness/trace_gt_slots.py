import sys, os
sys.path.insert(0, os.path.expanduser('~/Desktop/Projects/prylint/harness'))
import dump_infer
import astroid
from astroid.nodes.scoped_nodes.scoped_nodes import ClassDef
orig_slots = ClassDef._slots
def traced_slots(self):
    sys.stderr.write(f'SLOTSOF {self.qname()}\n')
    return orig_slots(self)
ClassDef._slots = traced_slots
# _all_slots is a cached_property; wrap the underlying function
import astroid.nodes.scoped_nodes.scoped_nodes as sn
orig_all = ClassDef._all_slots.func if hasattr(ClassDef._all_slots, 'func') else None
if orig_all:
    from functools import cached_property
    def traced_all(self):
        sys.stderr.write(f'ALLSLOTS {self.qname()}\n')
        return orig_all(self)
    ClassDef._all_slots = cached_property(traced_all)
    ClassDef._all_slots.__set_name__(ClassDef, '_all_slots')
dump_infer.main()
