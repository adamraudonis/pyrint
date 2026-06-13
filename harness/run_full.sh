#!/bin/bash
# Usage: run_full.sh <corpus> <hook|full> -- run prylint with a profile (bash word-splitting)
set -u
ROOT=~/Desktop/Projects/prylint
C="$1"; P="$2"
cd $ROOT/corpora/$C || exit 1
FLAGS=$(sed "s|HARNESS_EMPTY_RC|$ROOT/harness/empty.rcfile|" $ROOT/harness/flags_$P.txt)
# Isolate PYLINT_HOME to a fresh empty dir per run, EXACTLY as the Phase-F GT
# (gt_iso.sh) does: the stats cache is then empty, so the footer has no
# "(previous run: ...)" suffix. Without isolation prylint would read the user's
# live ~/Library/Caches/pylint and print a spurious previous-run suffix.
PLH=$(mktemp -d /tmp/prylintrun.XXXXXX)
export PRYLINT_PYTHON=$ROOT/.venv-pylint/bin/python PYLINTHOME=$PLH PYTHONHASHSEED=0
$ROOT/target/release/prylint . $FLAGS > $ROOT/harness/results/$C.$P.ours 2>/dev/null
echo $? > $ROOT/harness/results/$C.$P.ours.exit
rm -rf "$PLH"
