#!/bin/bash
# Usage: run_full.sh <corpus> <hook|full> -- run prylint with a profile (bash word-splitting)
set -u
ROOT=~/Desktop/Projects/prylint
C="$1"; P="$2"
cd $ROOT/corpora/$C || exit 1
FLAGS=$(sed "s|HARNESS_EMPTY_RC|$ROOT/harness/empty.rcfile|" $ROOT/harness/flags_$P.txt)
# Isolate PYLINT_HOME to a fresh empty dir per (corpus,profile), using the SAME
# deterministic path as the GT (gt_iso.sh): the cache is empty so the footer has
# no "(previous run: ...)" suffix, AND any F0002 crash-template path shares the
# GT's PYLINTHOME prefix so only the wall-clock timestamp differs (bytecmp.py
# normalizes that). Without isolation prylint would read the user's live cache.
PLH=/tmp/prylint-plh-$C-$P
rm -rf "$PLH"; mkdir -p "$PLH"
export PRYLINT_PYTHON=$ROOT/.venv-pylint/bin/python PYLINTHOME=$PLH PYTHONHASHSEED=0
$ROOT/target/release/prylint . $FLAGS > $ROOT/harness/results/$C.$P.ours 2>/dev/null
echo $? > $ROOT/harness/results/$C.$P.ours.exit
rm -rf "$PLH"
