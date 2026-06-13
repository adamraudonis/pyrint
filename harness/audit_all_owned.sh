#!/bin/bash
# Run check_owned_f.sh across all 27 corpora x 2 profiles, summarizing.
ROOT=~/Desktop/Projects/prylint
CORPORA="scrapy rich werkzeug tornado celery fastapi pip botocore mypy pydantic matplotlib ansible twisted sqlalchemy zulip django salt scikit-learn nova numpy sentry pandas sympy airflow pylfunc core black"
for c in $CORPORA; do
  for p in hook full; do
    res=$(bash "$ROOT/harness/check_owned_f.sh" "$c" "$p" 2>&1)
    first=$(echo "$res" | head -1)
    echo "[$c.$p] $first"
    # print FN/FP code breakdown lines if present
    echo "$res" | grep -E '^  (FN|FP):' | sed 's/^/    /'
  done
done
