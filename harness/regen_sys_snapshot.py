import sys, os, json
# Regenerate crates/pyinfer/snapshot/sys.json in a process whose state
# matches a canonical `python dump_infer.py <root> <items>` oracle run at
# the moment astroid raw-builds the `sys` module (lazily, during prebuild):
#   - sys.modules: dump_infer's import set ONLY (dump_infer itself is
#     __main__ there, so not in sys.modules)
#   - sys.path: [<script dir = harness>, *venv defaults] — the oracle's
#     main() additionally inserts realpath(corpus root) at position 0,
#     which is corpus-dependent: the ENGINE prepends it at snapshot load
#     instead (snapshot.rs sys.path patch).
# Run as:
#   cd harness && ../.venv-pylint/bin/python -E regen_sys_snapshot.py
assert sys.flags.ignore_environment, "run with python -E (no PYTHONPATH pollution)"
HARNESS = os.path.dirname(os.path.abspath(__file__))
assert os.path.realpath(sys.path[0]) == os.path.realpath(HARNESS), \
    "run as a script from the harness dir (sys.path[0] must be harness)"
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
out = os.path.join(os.path.dirname(HARNESS), 'crates', 'pyinfer', 'snapshot', 'sys.json')
with open(out, 'w', encoding='utf-8') as f:
    json.dump(result, f, separators=(',', ':'))
print('written', os.path.getsize(out))
