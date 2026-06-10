#!/bin/bash
# Usage: run_prylint.sh <corpus> -- runs our binary with the exact target flags
# and writes harness/results/<corpus>.ours.{out,exit,time}
set -u
ROOT=~/Desktop/Projects/prylint
C="$1"
FLAGS=$(cat $ROOT/harness/flags.txt)
OUT=$ROOT/harness/results/$C.ours
export PRYLINT_PYTHON=$ROOT/.venv-pylint/bin/python
cd $ROOT/corpora/$C || exit 1
START=$(python3 -c 'import time; print(time.time())')
$ROOT/target/release/prylint . $FLAGS > $OUT.out 2> $OUT.err
echo $? > $OUT.exit
END=$(python3 -c 'import time; print(time.time())')
python3 -c "print(f'{$END-$START:.2f}')" > $OUT.time
echo "$C: $(cat $OUT.time)s exit=$(cat $OUT.exit) lines=$(wc -l < $OUT.out | tr -d ' ')"
