#!/bin/bash
# Round 8 FP sweep: run all corpora both profiles, report bytecmp2 parity + FP codes.
set -u
ROOT=~/Desktop/Projects/prylint
cd $ROOT
CORPORA="scrapy rich werkzeug tornado celery fastapi black pip botocore mypy pydantic matplotlib ansible twisted sqlalchemy zulip django salt scikit-learn nova numpy sentry pandas sympy airflow pylfunc core"
MODE="${1:-check}"   # check = just bytecmp existing .ours; run = regenerate .ours first
for c in $CORPORA; do
  for p in hook full; do
    g=harness/results/$c.$p.out
    o=harness/results/$c.$p.ours
    if [ "$MODE" = run ]; then
      bash harness/run_full.sh "$c" "$p" >/dev/null 2>&1
    fi
    if [ ! -f "$g" ]; then echo "$c.$p: MISSING GT"; continue; fi
    if [ ! -f "$o" ]; then echo "$c.$p: MISSING OURS"; continue; fi
    if python3 harness/bytecmp2.py "$g" "$o" --drop-no-member >/tmp/bc.$c.$p 2>&1; then
      echo "$c.$p: OK"
    else
      echo "$c.$p: DIFF"
    fi
  done
done
