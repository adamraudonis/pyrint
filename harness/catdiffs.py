#!/usr/bin/env python3
"""Categorize inference-dump diffs after a check_inferdump.sh run.

Reads /tmp/inferlist_<corpus>.jsonl, /tmp/inferdump_rs_<corpus>.out and the
cache; prints per-line (gt, ours) pairs grouped by a coarse value-pattern
signature, sorted by volume. Usage: catdiffs.py <corpus> [--full]
"""
import json, os, re, sys
from collections import Counter, defaultdict

ROOT = os.path.expanduser('~/Desktop/Projects/prylint')
corpus = sys.argv[1]
full = '--full' in sys.argv
cache = f'{ROOT}/harness/infercache/{corpus}'
items = [json.loads(l) for l in open(f'/tmp/inferlist_{corpus}.jsonl') if l.strip()]
rs, cur, buf = {}, None, []
for line in open(f'/tmp/inferdump_rs_{corpus}.out', encoding='utf-8', errors='replace'):
    if line.startswith('=== '):
        if cur is not None: rs[cur] = ''.join(buf)
        cur, buf = line[4:].strip(), []
    elif cur is not None:
        buf.append(line)
if cur is not None: rs[cur] = ''.join(buf)

def sig_of_vals(s):
    out = []
    for v in s.split(' | '):
        v = v.split(':')[0]
        out.append(v)
    return ','.join(out)

def relation(a, b):
    """Structural relation between GT values and our values."""
    av, bv = a.split(' | '), b.split(' | ')
    if av == bv: return 'EQ'
    la, lb = len(av), len(bv)
    # common prefix / suffix
    p = 0
    while p < min(la, lb) and av[p] == bv[p]: p += 1
    sfx = 0
    while sfx < min(la, lb) - p and av[la-1-sfx] == bv[lb-1-sfx]: sfx += 1
    gmid, omid = av[p:la-sfx], bv[p:lb-sfx]
    gs = ','.join(x.split(':')[0] for x in gmid)
    os_ = ','.join(x.split(':')[0] for x in omid)
    return f'mid GT[{gs}] RS[{os_}] @p{p}/s{sfx}'

def keyof(line):
    # LINE format: "<lineno>:<col>:<Kind> -> v1 | v2 | ..."
    m = re.match(r'^(\d+):(\d+):(\w+) -> (.*)$', line)
    if not m: return ('OTHER', line.strip()[:60])
    ln, col, kind, vals = m.groups()
    return (kind, sig_of_vals(vals))

cats = Counter()
examples = defaultdict(list)
for it in items:
    f = it['path']
    p = os.path.join(cache, f.lstrip('./') + '.dump')
    try: gt = open(p, encoding='utf-8', errors='replace').read()
    except FileNotFoundError: continue
    ours = rs.get(f, '')
    if gt == ours: continue
    gtl, ol = gt.splitlines(), ours.splitlines()
    for i, (a, b) in enumerate(zip(gtl, ol)):
        if a == b: continue
        ka, kb = keyof(a), keyof(b)
        ma = re.match(r'^(\d+):(\d+):(\w+) -> (.*)$', a)
        mb = re.match(r'^(\d+):(\d+):(\w+) -> (.*)$', b)
        if ma and mb and ma.group(1, 2, 3) == mb.group(1, 2, 3):
            cat = f'{ka[0]} {relation(ma.group(4), mb.group(4))}'
        else:
            cat = f'MISALIGN GT[{ka[0]}:{ka[1]}] RS[{kb[0]}:{kb[1]}]'
        cats[cat] += 1
        if len(examples[cat]) < 8:
            examples[cat].append((f, a, b))
    if len(gtl) != len(ol):
        cat = f'LENGTH gt={len(gtl)} ours={len(ol)}'
        cats[cat] += abs(len(gtl)-len(ol))
        if len(examples[cat]) < 8: examples[cat].append((f, '', ''))

for cat, n in cats.most_common(40 if not full else None):
    print(f'--- {n:5d}  {cat}')
    for f, a, b in examples[cat][:4]:
        print(f'      {f}')
        print(f'      GT  {a.strip()[:220]}')
        print(f'      RS  {b.strip()[:220]}')
