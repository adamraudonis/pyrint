#!/bin/bash
# Usage: run_prylint2.sh <corpus> <profile: hook|full> — run our binary with
# the full-mode profile flags; writes harness/results/<corpus>.ours<profile>.{out,exit}
set -u
ROOT=~/Desktop/Projects/prylint
C="$1"; P="$2"
FLAGS=$(sed "s|HARNESS_EMPTY_RC|$ROOT/harness/empty.rcfile|" $ROOT/harness/flags_$P.txt)
OUT=$ROOT/harness/results/$C.ours$P
export PRYLINT_PYTHON=$ROOT/.venv-pylint/bin/python
export PRYLINT_ALLOW_PARTIAL=1
cd $ROOT/corpora/$C || exit 1
$ROOT/target/release/prylint . $FLAGS > $OUT.out 2> $OUT.err
echo $? > $OUT.exit
echo "$C/$P: exit=$(cat $OUT.exit) lines=$(wc -l < $OUT.out | tr -d ' ')"
