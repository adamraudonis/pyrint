#!/bin/bash
# Round 9 FP sweep: run ours for every (corpus,profile), then bytecmp2 gate +
# diffmsg FP triage (filtering out the sanctioned no-member family E1101/I1101).
set -u
ROOT=~/Desktop/Projects/prylint
CORPORA="scrapy rich werkzeug tornado celery fastapi black pip botocore mypy pydantic matplotlib ansible twisted sqlalchemy zulip django salt scikit-learn nova numpy sentry pandas sympy airflow pylfunc core"
OUT=/tmp/round9_sweep.txt
: > $OUT
for c in $CORPORA; do
  for p in hook full; do
    bash $ROOT/harness/run_full.sh $c $p
    o=$ROOT/harness/results/$c.$p.out
    u=$ROOT/harness/results/$c.$p.ours
    python3 $ROOT/harness/bytecmp2.py "$o" "$u" --drop-no-member >/tmp/bc2_$c.$p.txt 2>&1
    gate=$?
    # diffmsg FP, filtering no-member
    python3 $ROOT/harness/diffmsg.py "$o" "$u" >/tmp/dm_$c.$p.txt 2>&1
    # extract real FP count (total FP minus E1101/I1101)
    fp_real=$(python3 - "$o" "$u" <<'PY'
import re,sys
from collections import Counter
LINE=re.compile(r"^([^:]+):(\d+):(\d+): ([A-Z]\d{4}): (.*)$")
def parse(p):
    out=[]
    for raw in open(p,encoding="utf-8",errors="replace"):
        l=raw.rstrip("\n")
        m=LINE.match(l)
        if m: out.append((m.group(1),m.group(2),m.group(3),m.group(4),m.group(5)))
    return out
gt=Counter(parse(sys.argv[1])); cand=Counter(parse(sys.argv[2]))
fp=cand-gt
NOMEM={"E1101","I1101"}
# canonicalize R0801 to a count (nondeterministic block); ignore exact lines
real=Counter()
for m,n in fp.items():
    if m[3] in NOMEM: continue
    if m[3]=="R0801": continue  # nondeterministic, gate handles via count
    real[m[3]]+=n
print(sum(real.values()), dict(real))
PY
)
    echo "$c.$p gate=$gate fp_real=$fp_real" >> $OUT
  done
done
echo "DONE" >> $OUT
