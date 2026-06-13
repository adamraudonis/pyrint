#!/bin/bash
# Regenerate HOOK-profile GT for all 27 corpora with isolated empty PYLINTHOME
# (clean footers). Hook profile is deterministic (R0801 disabled). Skips black
# until last (slow). Writes <corpus>.hook.{out,exit,time}.
set -u
ROOT=~/Desktop/Projects/prylint
ORDER="pylfunc werkzeug tornado rich scrapy botocore celery fastapi pydantic pip twisted matplotlib mypy ansible scikit-learn sqlalchemy numpy zulip pandas nova sympy django salt airflow sentry core black"
for c in $ORDER; do
  bash "$ROOT/harness/gt_iso.sh" "$c" hook
done
echo "GT_ISO_HOOK_DONE"
