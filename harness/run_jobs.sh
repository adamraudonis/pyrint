#!/bin/bash
# Usage: run_jobs.sh <corpus> <profile> <jobs> <tag>
# Runs prylint on corpus $1 with profile flags_$2.txt at -j $3, writing
# harness/results/<corpus>.<tag>.{out,err,exit}. Word-splitting of $FLAGS is
# intentional (no flag contains spaces), matching run_prylint2.sh.
set -u
ROOT=~/Desktop/Projects/prylint
C="$1"; P="$2"; J="$3"; TAG="$4"
FLAGS=$(sed "s|HARNESS_EMPTY_RC|$ROOT/harness/empty.rcfile|" $ROOT/harness/flags_$P.txt)
OUT=$ROOT/harness/results/$C.$TAG
export PRYLINT_PYTHON=$ROOT/.venv-pylint/bin/python
export PRYLINT_ALLOW_PARTIAL=1
cd $ROOT/corpora/$C || exit 1
$ROOT/target/release/prylint . $FLAGS -j "$J" > $OUT.out 2> $OUT.err
echo $? > $OUT.exit
echo "$C/$P -j$J [$TAG]: exit=$(cat $OUT.exit) lines=$(wc -l < $OUT.out | tr -d ' ')"
