#!/bin/bash
# Broad FP census across all 27 corpora x {full,hook}, excluding the no-member
# family (E1101/I1101), E0611, and F0002 crash-path. Prints any corpus/profile
# with a real (non-excluded) FALSE POSITIVE.
set -u
ROOT=~/Desktop/Projects/prylint
CORPORA="scrapy rich werkzeug tornado celery fastapi black pip botocore mypy pydantic matplotlib ansible twisted sqlalchemy zulip django salt scikit-learn nova numpy sentry pandas sympy airflow pylfunc core"
any=0
for prof in full hook; do
  echo "##### $prof #####"
  for c in $CORPORA; do
    gt="$ROOT/harness/results/$c.$prof.out"
    ours="$ROOT/harness/results/$c.$prof.ours"
    [ -f "$gt" ] && [ -f "$ours" ] || { echo "[$c]: MISSING results"; continue; }
    fp=$(python3 "$ROOT/harness/diffmsg.py" "$gt" "$ours" 2>/dev/null \
         | sed -n '/^FALSE POSITIVES/,/^$/p' \
         | grep -E '^  [A-Z][0-9]{4}:' \
         | grep -vE 'E1101:|I1101:|E0611:|F0002:')
    if [ -n "$fp" ]; then
      echo "[$c]: $(printf '%s' "$fp" | tr -s ' \n' ' ')"
      any=1
    fi
  done
done
[ "$any" -eq 0 ] && echo "CENSUS: ZERO real FPs on all 27 corpora both profiles" || echo "CENSUS: real FPs present (above)"
