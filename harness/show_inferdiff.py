#!/usr/bin/env python3
"""Show GT-vs-ours diff lines for one file after check_inferdump.sh <corpus>.
Usage: show_inferdiff.py <corpus> <relpath> [context]"""
import os, sys
corpus, relpath = sys.argv[1], sys.argv[2]
ctx = int(sys.argv[3]) if len(sys.argv) > 3 else 0
ROOT = os.path.expanduser('~/Desktop/Projects/prylint')
cache = os.path.join(ROOT, 'harness/infercache', corpus, relpath.lstrip('./') + '.dump')
gt = open(cache, encoding='utf-8', errors='replace').read()
rs, cur, buf = {}, None, []
for line in open(f'/tmp/inferdump_rs_{corpus}.out', encoding='utf-8', errors='replace'):
    if line.startswith('=== '):
        if cur is not None: rs[cur] = ''.join(buf)
        cur, buf = line[4:].strip(), []
    elif cur is not None:
        buf.append(line)
if cur is not None: rs[cur] = ''.join(buf)
ours = rs.get(relpath, rs.get('./' + relpath, ''))
gtl, ol = gt.splitlines(), ours.splitlines()
n = max(len(gtl), len(ol))
diff_idx = [i for i in range(n) if (gtl[i] if i < len(gtl) else None) != (ol[i] if i < len(ol) else None)]
shown = set()
for i in diff_idx:
    for j in range(max(0, i - ctx), min(n, i + ctx + 1)):
        shown.add(j)
for j in sorted(shown):
    a = gtl[j] if j < len(gtl) else '<missing>'
    b = ol[j] if j < len(ol) else '<missing>'
    if a != b:
        print(f'GT  {j}: {a}')
        print(f'OURS{j}: {b}')
    else:
        print(f'    {j}: {a}')
