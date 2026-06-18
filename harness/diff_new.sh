#!/bin/bash
# Byte-compare prylint -E vs pylint -E ground truth on the new corpora.
# Usage: diff_new.sh <corpus...>   (prints IDENTICAL / per-code FP+FN)
ROOT=~/Desktop/Projects/prylint
FLAGS=$(cat $ROOT/harness/flags.txt)
export PRYLINT_PYTHON=$ROOT/.venv-pylint/bin/python
for c in "$@"; do
  [ -f $ROOT/harness/results/$c.iso.out ] || { echo "$c: NO GROUND TRUTH"; continue; }
  cd $ROOT/corpora/$c || continue
  $ROOT/target/release/prylint . $FLAGS --rcfile=$ROOT/harness/empty.rcfile > $ROOT/harness/results/$c.ours.out 2>/dev/null
  echo $? > $ROOT/harness/results/$c.ours.exit
  if python3 $ROOT/harness/bytecmp2.py $ROOT/harness/results/$c.iso.out $ROOT/harness/results/$c.ours.out >/dev/null 2>&1 && [ "$(cat $ROOT/harness/results/$c.iso.exit)" = "$(cat $ROOT/harness/results/$c.ours.exit)" ]; then
    echo "$c: IDENTICAL ($(grep -cE ': [A-Z][0-9]{4}:' $ROOT/harness/results/$c.iso.out) msgs)"
  else
    d=$($ROOT/.venv-pylint/bin/python $ROOT/harness/diffmsg.py $ROOT/harness/results/$c.iso.out $ROOT/harness/results/$c.ours.out 2>/dev/null)
    fp=$(echo "$d" | grep "FALSE POS" | grep -oE '[0-9]+$'); fn=$(echo "$d" | grep "FALSE NEG" | grep -oE '[0-9]+$')
    echo "$c: DIFFERS  FP=${fp:-0} FN=${fn:-0}"
    echo "$d" | grep -E "^  [A-Z][0-9]" | head -8 | sed 's/^/    /'
  fi
done
