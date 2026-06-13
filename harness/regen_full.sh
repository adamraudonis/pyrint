#!/bin/bash
# Regenerate all full-profile ours results, ordered fastest-first.
set -u
ROOT=~/Desktop/Projects/prylint
ORDER="pylfunc scrapy rich werkzeug tornado celery pip mypy pydantic twisted sqlalchemy fastapi scikit-learn ansible sympy numpy nova django pandas salt matplotlib botocore zulip core airflow sentry black"
for c in $ORDER; do
  t0=$(date +%s)
  bash "$ROOT/harness/run_full.sh" "$c" full
  t1=$(date +%s)
  echo "FULLDONE $c $((t1-t0))s exit=$(cat "$ROOT/harness/results/$c.full.ours.exit")"
done
echo "FULL ALL DONE"
