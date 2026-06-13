#!/bin/bash
# Usage: gt_iso.sh <corpus> <profile: full|hook>
# Regenerate ground truth with an ISOLATED, EMPTY PYLINTHOME so the score
# footer carries NO "(previous run: ...)" suffix (the Phase-F protocol:
# profiles run against a fresh stats cache; --persistent=no never saves,
# --persistent=yes saves but the single fresh run has no prior to load).
# Writes <corpus>.<profile>.{out,exit,time}.
set -u
ROOT=~/Desktop/Projects/prylint
C="$1"; P="$2"
FLAGS=$(sed "s|HARNESS_EMPTY_RC|$ROOT/harness/empty.rcfile|" $ROOT/harness/flags_$P.txt)
OUT=$ROOT/harness/results/$C.$P
PLH=$(mktemp -d /tmp/gtiso.XXXXXX)
export PYTHONHASHSEED=0
export PYLINTHOME=$PLH
cd $ROOT/corpora/$C || exit 1
START=$(date +%s)
$ROOT/.venv-pylint/bin/pylint . $FLAGS > $OUT.out 2> $OUT.err
echo $? > $OUT.exit
END=$(date +%s)
echo $((END-START)) > $OUT.time
rm -rf "$PLH"
echo "$C/$P: $((END-START))s exit=$(cat $OUT.exit) lines=$(wc -l < $OUT.out | tr -d ' ')"
