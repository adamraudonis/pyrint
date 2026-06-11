#!/bin/bash
# For each new corpus: pylint ground truth (slow), then snapshot-binary run, then byte compare.
set -u
ROOT=~/Desktop/Projects/prylint
SUMMARY=$ROOT/harness/results/new_corpora_summary.txt
: > $SUMMARY
for c in "$@"; do
  $ROOT/harness/ground_truth.sh $c iso >> $SUMMARY 2>&1
  cd $ROOT/corpora/$c || continue
  FLAGS=$(cat $ROOT/harness/flags.txt)
  export PRYLINT_PYTHON=$ROOT/.venv-pylint/bin/python
  START=$(python3 -c 'import time; print(time.time())')
  /tmp/prylint-parity . $FLAGS > $ROOT/harness/results/$c.ours.out 2> $ROOT/harness/results/$c.ours.err
  echo $? > $ROOT/harness/results/$c.ours.exit
  END=$(python3 -c 'import time; print(time.time())')
  python3 -c "print(f'{$END-$START:.2f}')" > $ROOT/harness/results/$c.ours.time
  if cmp -s $ROOT/harness/results/$c.iso.out $ROOT/harness/results/$c.ours.out && [ "$(cat $ROOT/harness/results/$c.iso.exit)" = "$(cat $ROOT/harness/results/$c.ours.exit)" ]; then
    echo "$c: BYTE-IDENTICAL ($(cat $ROOT/harness/results/$c.ours.time)s vs $(cat $ROOT/harness/results/$c.iso.time)s, exit $(cat $ROOT/harness/results/$c.ours.exit))" >> $SUMMARY
  else
    echo "$c: DIFFERS (ours exit $(cat $ROOT/harness/results/$c.ours.exit) vs $(cat $ROOT/harness/results/$c.iso.exit))" >> $SUMMARY
    $ROOT/.venv-pylint/bin/python $ROOT/harness/diffmsg.py $ROOT/harness/results/$c.iso.out $ROOT/harness/results/$c.ours.out 2>/dev/null | head -25 >> $SUMMARY
  fi
done
echo "ALL DONE" >> $SUMMARY
