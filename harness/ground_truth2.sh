#!/bin/bash
# Usage: ground_truth2.sh <corpus> <profile: full|hook>
# Like ground_truth.sh but with a flags profile; writes <corpus>.<profile>.{out,exit,time}
set -u
ROOT=~/Desktop/Projects/prylint
C="$1"; P="$2"
FLAGS=$(sed "s|HARNESS_EMPTY_RC|$ROOT/harness/empty.rcfile|" $ROOT/harness/flags_$P.txt)
OUT=$ROOT/harness/results/$C.$P
export PYTHONHASHSEED=0
cd $ROOT/corpora/$C || exit 1
START=$(date +%s)
$ROOT/.venv-pylint/bin/pylint . $FLAGS > $OUT.out 2> $OUT.err
echo $? > $OUT.exit
END=$(date +%s)
echo $((END-START)) > $OUT.time
echo "$C/$P: $((END-START))s exit=$(cat $OUT.exit) lines=$(wc -l < $OUT.out | tr -d ' ')"
