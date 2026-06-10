#!/usr/bin/env python3
"""Split GT/RS infer traces into top-level segments, normalize, diff chosen pair.

Usage: seg_diff.py <gt.trace> <rs.trace> [seg_index_from_end] [--list]
Normalization: keep '> Kind name ln=' and 'yield VAL' lines (depth preserved
as indentation), strip ni=/cc=/bn=/HIT/MISS noise, unify ln= rendering.
"""
import re
import sys


def segments(path):
    raw = [l.rstrip('\n') for l in open(path, encoding='utf-8', errors='replace')]
    lines = [l for l in raw if l.lstrip().startswith(('> ', 'yield '))]
    if not lines:
        return []
    minind = min(len(l) - len(l.lstrip(' ')) for l in lines if l.lstrip().startswith('> '))
    segs, cur = [], None
    for l in lines:
        ind = len(l) - len(l.lstrip(' '))
        if ind == minind and l.lstrip().startswith('> '):
            cur = []
            segs.append(cur)
        if cur is not None:
            cur.append(l[minind:] if ind >= minind else l)
    return segs


def norm_line(l):
    ind = len(l) - len(l.lstrip(' '))
    s = l.strip()
    if s.startswith('> '):
        m = re.match(r'> (\S+) (\S*) ?ln=(\S+)', s)
        if not m:
            return None
        k, nm, ln = m.groups()
        mm = re.match(r'Some\("(.*)"\)', ln)
        if mm:
            ln = f"'{mm.group(1)}'"
        if ln == 'None':
            ln = 'None'
        if k in ('Const', 'Tuple', 'List', 'Set', 'Dict'):
            nm = ''
        return ' ' * ind + f'> {k} {nm} ln={ln}'
    if s.startswith('yield '):
        s = re.sub(r' ni=-?\d+$', '', s)
        return ' ' * ind + s
    return None


def main():
    g = segments(sys.argv[1])
    r = segments(sys.argv[2])
    if '--list' in sys.argv:
        n = max(len(g), len(r))
        for i in range(1, min(n, 40) + 1):
            a = g[-i][0].strip()[:55] if i <= len(g) else '-'
            b = r[-i][0].strip()[:55] if i <= len(r) else '-'
            la = len(g[-i]) if i <= len(g) else 0
            lb = len(r[-i]) if i <= len(r) else 0
            print(f'-{i}: {a} ({la}) || {b} ({lb})')
        return
    idx = int(sys.argv[3]) if len(sys.argv) > 3 else 1
    ga = [norm_line(l) for l in g[-idx]]
    rb = [norm_line(l) for l in r[-idx]]
    ga = [x for x in ga if x]
    rb = [x for x in rb if x]
    open('/tmp/seg_gt.txt', 'w').write('\n'.join(ga) + '\n')
    open('/tmp/seg_rs.txt', 'w').write('\n'.join(rb) + '\n')
    print(f'gt seg {len(ga)} lines, rs seg {len(rb)} lines -> /tmp/seg_gt.txt /tmp/seg_rs.txt')


if __name__ == '__main__':
    main()
