#!/bin/bash
# Regenerate full-profile .ours captures for all 27 corpora.
ROOT=~/Desktop/Projects/prylint
CORPORA="scrapy rich werkzeug tornado celery fastapi pip botocore mypy pydantic matplotlib ansible twisted sqlalchemy zulip django salt scikit-learn nova numpy sentry pandas sympy airflow pylfunc core black"
for c in $CORPORA; do
  "$ROOT/harness/run_full.sh" "$c" full </dev/null >/dev/null 2>&1
  echo "$c full exit=$(cat "$ROOT/harness/results/$c.full.ours.exit")"
done
