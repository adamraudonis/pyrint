#!/bin/bash
# -E 27-corpus byte parity gate.
set -u
ROOT=~/Desktop/Projects/prylint
CORPORA="scrapy rich werkzeug tornado celery fastapi black pip botocore mypy pydantic matplotlib ansible twisted sqlalchemy zulip django salt scikit-learn nova numpy sentry pandas sympy airflow pylfunc core"
fail=0
for c in $CORPORA; do
  bash "$ROOT/harness/run_prylint.sh" "$c" >/dev/null 2>&1
  if python3 "$ROOT/harness/bytecmp.py" "$ROOT/harness/results/$c.iso.out" "$ROOT/harness/results/$c.ours.out"; then
    echo "$c: EQUAL"
  else
    echo "$c: DIFF !!!"
    fail=1
  fi
done
[ "$fail" -eq 0 ] && echo "EGATE ALL EQUAL" || echo "EGATE FAILED"
