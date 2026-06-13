#!/bin/bash
# Usage: run_full.sh <corpus> <hook|full> -- run prylint with a profile (bash word-splitting)
set -u
ROOT=~/Desktop/Projects/prylint
C="$1"; P="$2"
cd $ROOT/corpora/$C || exit 1
FLAGS=$(sed "s|HARNESS_EMPTY_RC|$ROOT/harness/empty.rcfile|" $ROOT/harness/flags_$P.txt)
export PRYLINT_PYTHON=$ROOT/.venv-pylint/bin/python PRYLINT_ALLOW_PARTIAL=1
$ROOT/target/release/prylint . $FLAGS > $ROOT/harness/results/$C.$P.ours 2>/dev/null
echo $? > $ROOT/harness/results/$C.$P.ours.exit
