#!/bin/bash
# Permanent regression suite for user-reported discrepancies. Each
# harness/regressions/<name>.py has a <name>.expected (captured from pinned
# pylint 4.0.5 -E). prylint must reproduce it byte-for-byte. Self-contained
# fixtures need no third-party deps; *.needs-venv-repro fixtures require the
# sqlalchemy-1.4 repro venv (skipped if absent).
set -u
ROOT=~/Desktop/Projects/prylint
cd $ROOT/harness/regressions || exit 1
P=$ROOT/target/release/prylint
FAIL=0
for exp in *.expected; do
  name="${exp%.expected}"
  [ -f "$name.py" ] || continue
  PYTHONHASHSEED=0 "$P" -E "$name.py" 2>/dev/null > "/tmp/reg_$name.out"
  if diff -q "$exp" "/tmp/reg_$name.out" >/dev/null; then
    echo "PASS $name"
  else
    echo "FAIL $name"; diff "$exp" "/tmp/reg_$name.out" | head -8; FAIL=1
  fi
done
exit $FAIL
