import sys, os, json
# mimic dump_infer.py's import set exactly: it imports json, os, sys, astroid
sys.path.insert(0, '/Users/adamraudonis/Desktop/Projects/prylint/harness')
import dump_infer  # noqa  (imports json/os/sys/astroid + MANAGER, bases, nodes, objects, util)
# the oracle process argv: [script, root, items.jsonl]
sys.argv = ['dump_infer.py', '.', 'items.jsonl']
import gen_snapshot
del sys.modules['gen_snapshot']  # not present in the oracle process
del sys.modules['dump_infer']  # the oracle runs dump_infer.py AS __main__
from astroid import MANAGER
from astroid.builder import AstroidBuilder
AstroidBuilder(MANAGER)
result = gen_snapshot.snapshot_module('sys')
assert result not in (None, 'source')
out = '/Users/adamraudonis/Desktop/Projects/prylint/crates/pyinfer/snapshot/sys.json'
with open(out, 'w', encoding='utf-8') as f:
    json.dump(result, f, separators=(',', ':'))
print('written', os.path.getsize(out))
