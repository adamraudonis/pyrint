#!/bin/bash
set -u
ROOT=~/Desktop/Projects/prylint
cd $ROOT
# regenerate the DIFF corpora fresh
for pair in "pip full" "sqlalchemy hook" "sqlalchemy full" "scikit-learn full" "nova full" "sympy full" "pylfunc full" "core full"; do
  set -- $pair
  c=$1; p=$2
  bash harness/run_full.sh "$c" "$p" >/dev/null 2>&1
  if python3 harness/bytecmp2.py harness/results/$c.$p.out harness/results/$c.$p.ours --drop-no-member >/tmp/bc.$c.$p 2>&1; then
    echo "$c.$p: OK"
  else
    echo "$c.$p: DIFF"
  fi
done
