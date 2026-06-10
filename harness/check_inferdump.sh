#!/bin/bash
# Usage: check_inferdump.sh <corpus> [N|all]
# Compare astroid inference dumps (cached) vs `prylint --dump-infer`.
#
# Ground truth is cached per file under harness/infercache/<corpus>/ and is
# produced by ONE dump_infer.py run per corpus (it must prebuild every target
# module, mirroring pylint phase 1, so caching per-invocation is pointless).
# Warm with: harness/warm_infercache.sh <corpus>
ROOT=~/Desktop/Projects/prylint
C=$ROOT/corpora/$1
N=${2:-200}
CACHE=$ROOT/harness/infercache/$1
cd "$C" || exit 1
if [ ! -d "$CACHE" ]; then
  echo "infercache for $1 missing -- run harness/warm_infercache.sh $1 first" >&2
  exit 2
fi
$ROOT/target/release/prylint . --dump-fileitems > /tmp/inferitems_$1.jsonl 2>/dev/null
if [ "$N" = "all" ]; then
  cp /tmp/inferitems_$1.jsonl /tmp/inferlist_$1.jsonl
else
  head -$N /tmp/inferitems_$1.jsonl > /tmp/inferlist_$1.jsonl
fi
# rust side: ALWAYS prebuild the full fileitem list (mirrors pylint phase 1 /
# the oracle's warm run), but only dump the sampled subset via PRYLINT_DUMP_ONLY.
python3 -c "import json,sys; [print(json.loads(l)['path']) for l in open('/tmp/inferlist_$1.jsonl') if l.strip()]" > /tmp/inferonly_$1.txt
PRYLINT_DUMP_ONLY=/tmp/inferonly_$1.txt $ROOT/target/release/prylint . --dump-infer /tmp/inferitems_$1.jsonl > /tmp/inferdump_rs_$1.out 2>/dev/null
python3 - "$1" "$CACHE" <<'PYEOF'
import json, os, sys
corpus, cache = sys.argv[1], sys.argv[2]
items = [json.loads(l) for l in open(f'/tmp/inferlist_{corpus}.jsonl') if l.strip()]
rs, cur, buf = {}, None, []
for line in open(f'/tmp/inferdump_rs_{corpus}.out', encoding='utf-8', errors='replace'):
    if line.startswith('=== '):
        if cur is not None: rs[cur] = ''.join(buf)
        cur, buf = line[4:].strip(), []
    elif cur is not None:
        buf.append(line)
if cur is not None: rs[cur] = ''.join(buf)
diffs, total_lines, diff_lines = [], 0, 0
for it in items:
    f = it['path']
    p = os.path.join(cache, f.lstrip('./') + '.dump')
    try:
        gt = open(p, encoding='utf-8', errors='replace').read()
    except FileNotFoundError:
        continue
    ours = rs.get(f, '')
    total_lines += gt.count('\n')
    if gt != ours:
        diffs.append(f)
        gtl, ol = gt.splitlines(), ours.splitlines()
        diff_lines += sum(1 for a, b in zip(gtl, ol) if a != b) + abs(len(gtl) - len(ol))
with open(f'/tmp/inferdiffs_{corpus}.txt', 'w') as fh:
    fh.writelines('DIFF ' + d + '\n' for d in diffs)
print(f'checked: {len(items)} files, {total_lines} inference lines')
print(f'differing files: {len(diffs)} ({diff_lines} lines)')
for d in diffs[:15]: print('DIFF', d)
PYEOF
