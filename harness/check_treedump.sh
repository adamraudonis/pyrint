#!/bin/bash
# Usage: check_treedump.sh <corpus> [N|all]
# Compare astroid vs rust tree dumps on the first N files (default 200).
#
# The astroid (python) side is CACHED under harness/treecache/<corpus>/ --
# computed once per file, reused forever (corpora are pinned read-only).
# Each invocation therefore costs: rust dump (fast) + diff, plus a one-time
# python dump for files not yet cached (8-way parallel).
#
# SYNTAXERROR lines are compared by marker only: exact message text and
# CPython's line/offset are owned by the E0001 subsystem, not this harness.
ROOT=~/Desktop/Projects/prylint
C=$ROOT/corpora/$1
N=${2:-200}
CACHE=$ROOT/harness/treecache/$1
mkdir -p "$CACHE"
cd "$C" || exit 1
if [ "$N" = "all" ]; then
  find . \( -name '*.py' -o -name '*.pyi' \) | sort > /tmp/treelist_$1.txt
else
  find . \( -name '*.py' -o -name '*.pyi' \) | sort | head -$N > /tmp/treelist_$1.txt
fi

# 1) python side: dump only files missing from the cache
python3 - "$1" "$CACHE" <<'PYEOF'
import concurrent.futures as cf
import os, subprocess, sys
corpus, cache = sys.argv[1], sys.argv[2]
root = os.path.expanduser('~/Desktop/Projects/prylint')
files = [l.strip() for l in open(f'/tmp/treelist_{corpus}.txt') if l.strip()]
missing = [f for f in files if not os.path.exists(os.path.join(cache, f.lstrip('./') + '.dump'))]
if missing:
    print(f'caching {len(missing)} ground-truth dumps...', flush=True)
    def run(batch):
        py = os.path.join(root, '.venv-pylint/bin/python')
        out = subprocess.run([py, os.path.join(root, 'harness/dump_ast.py'), *batch],
                             capture_output=True, text=True).stdout
        cur, buf, sections = None, [], {}
        for line in out.splitlines(keepends=True):
            if line.startswith('=== '):
                if cur is not None: sections[cur] = ''.join(buf)
                cur, buf = line[4:].strip(), []
            elif cur is not None:
                buf.append(line)
        if cur is not None: sections[cur] = ''.join(buf)
        for f, content in sections.items():
            p = os.path.join(cache, f.lstrip('./') + '.dump')
            os.makedirs(os.path.dirname(p), exist_ok=True)
            with open(p, 'w') as fh: fh.write(content)
    batches = [missing[i:i+100] for i in range(0, len(missing), 100)]
    with cf.ThreadPoolExecutor(8) as ex:
        list(ex.map(run, batches))
PYEOF

# 2) rust side: xargs-batched (fast), single output stream
xargs -n 500 $ROOT/target/release/prylint --dump-ast < /tmp/treelist_$1.txt > /tmp/treedump_rs_$1.out 2>/dev/null

# 3) compare against cache
python3 - "$1" "$CACHE" <<'PYEOF'
import os, sys
corpus, cache = sys.argv[1], sys.argv[2]
files = [l.strip() for l in open(f'/tmp/treelist_{corpus}.txt') if l.strip()]
rs, cur, buf = {}, None, []
for line in open(f'/tmp/treedump_rs_{corpus}.out', encoding='utf-8', errors='replace'):
    if line.startswith('=== '):
        if cur is not None: rs[cur] = ''.join(buf)
        cur, buf = line[4:].strip(), []
    elif cur is not None:
        buf.append(line)
if cur is not None: rs[cur] = ''.join(buf)

def norm(text):
    if text is None: return None
    out = []
    for line in text.splitlines(keepends=True):
        if line.startswith('SYNTAXERROR'):
            line = 'SYNTAXERROR\n'
        out.append(line)
    return ''.join(out)

diffs = []
for f in files:
    p = os.path.join(cache, f.lstrip('./') + '.dump')
    try:
        gt = open(p, encoding='utf-8', errors='replace').read()
    except FileNotFoundError:
        diffs.append(f); continue
    if norm(gt) != norm(rs.get(f)):
        diffs.append(f)
with open(f'/tmp/treediffs_{corpus}.txt', 'w') as fh:
    fh.writelines('DIFF ' + d + '\n' for d in diffs)
print(f'checked: {len(files)}')
print(f'differing: {len(diffs)}')
for d in diffs[:20]: print('DIFF', d)
PYEOF
