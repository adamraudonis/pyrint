#!/bin/bash
# Usage: run_prylint.sh <corpus> -- runs our binary with the exact target flags
# and writes harness/results/<corpus>.ours.{out,exit,time}
set -u
ROOT=~/Desktop/Projects/prylint
C="$1"
# The iso ground truth (ground_truth.sh, variant=iso) was generated with
# --rcfile=empty.rcfile, which STOPS pylint's default-config discovery. prylint
# now implements that discovery (Phase F), so the -E runner must pass the same
# empty rcfile or it would pick up a corpus's own pyproject/[tool.pylint] config
# (e.g. scrapy enables useless-suppression) — diverging from the iso GT. This
# matches the GT invocation exactly; it does not relax the comparison.
FLAGS=$(cat $ROOT/harness/flags.txt)
OUT=$ROOT/harness/results/$C.ours
export PRYLINT_PYTHON=$ROOT/.venv-pylint/bin/python
cd $ROOT/corpora/$C || exit 1
START=$(python3 -c 'import time; print(time.time())')
$ROOT/target/release/prylint . $FLAGS --rcfile=$ROOT/harness/empty.rcfile > $OUT.out 2> $OUT.err
echo $? > $OUT.exit
END=$(python3 -c 'import time; print(time.time())')
python3 -c "print(f'{$END-$START:.2f}')" > $OUT.time
echo "$C: $(cat $OUT.time)s exit=$(cat $OUT.exit) lines=$(wc -l < $OUT.out | tr -d ' ')"
