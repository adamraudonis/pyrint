#!/bin/bash
# Usage: ground_truth.sh <corpus-name> [iso|native]
set -u
# PINNED: pylint output is hash-seed dependent where it iterates Python sets
# (proven: typecheck.py E1132 iterates CallSite.duplicated_keywords, a
# set[str] — order varies run-to-run under hash randomization). Ground truth
# is pinned at PYTHONHASHSEED=0; prylint replicates CPython's seed-0 string
# hash (siphash13, zero key) + set table order where pylint-visible.
export PYTHONHASHSEED=0
ROOT=~/Desktop/Projects/prylint
CORPUS="$1"; VARIANT="${2:-iso}"
FLAGS=$(cat $ROOT/harness/flags.txt)
PYLINT=$ROOT/.venv-pylint/bin/pylint
OUT=$ROOT/harness/results/$CORPUS.$VARIANT
cd $ROOT/corpora/$CORPUS || exit 1
EXTRA=""
[ "$VARIANT" = "iso" ] && EXTRA="--rcfile=$ROOT/harness/empty.rcfile"
START=$(date +%s)
$PYLINT . $FLAGS $EXTRA > $OUT.out 2> $OUT.err
echo $? > $OUT.exit
END=$(date +%s)
echo $((END-START)) > $OUT.time
echo "$CORPUS/$VARIANT: $(($END-$START))s exit=$(cat $OUT.exit) lines=$(wc -l < $OUT.out)"
