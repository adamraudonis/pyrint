#!/bin/bash
# Usage: warm_infercache.sh <corpus> -- one full dump_infer run, split per file
set -u
ROOT=~/Desktop/Projects/prylint
C="$1"
CACHE=$ROOT/harness/infercache/$C
mkdir -p "$CACHE"
cd $ROOT/corpora/$C || exit 1
$ROOT/target/release/prylint . --dump-fileitems > /tmp/warmitems_$C.jsonl 2>/dev/null
$ROOT/.venv-pylint/bin/python $ROOT/harness/dump_infer.py $ROOT/corpora/$C /tmp/warmitems_$C.jsonl 2>/tmp/warm_$C.err > /tmp/warm_$C.out
python3 - "$C" "$CACHE" <<'PYEOF'
import os, sys
corpus, cache = sys.argv[1], sys.argv[2]
cur, buf = None, []
def flush():
    if cur is None: return
    p = os.path.join(cache, cur.lstrip('./') + '.dump')
    os.makedirs(os.path.dirname(p), exist_ok=True)
    with open(p, 'w') as fh: fh.write(''.join(buf))
for line in open(f'/tmp/warm_{corpus}.out', encoding='utf-8', errors='replace'):
    if line.startswith('=== '):
        flush(); cur, buf = line[4:].strip(), []
    elif cur is not None:
        buf.append(line)
flush()
print(f'{corpus}: cached')
PYEOF
