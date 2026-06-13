#!/bin/bash
# Run prylint on corpus $1 with profile $2 (hook|full) -> results/$1.ours$2.*
set -u
ROOT=~/Desktop/Projects/prylint
C="$1"; P="$2"
cd $ROOT/corpora/$C
FLAGS=$(sed "s|HARNESS_EMPTY_RC|$ROOT/harness/empty.rcfile|" $ROOT/harness/flags_$P.txt)
PRYLINT_ALLOW_PARTIAL=1 $ROOT/target/release/prylint . $FLAGS \
  > $ROOT/harness/results/$C.ours$P.out 2> $ROOT/harness/results/$C.ours$P.err
echo $? > $ROOT/harness/results/$C.ours$P.exit
