#!/bin/bash
set -u
ROOT=~/Desktop/Projects/prylint
cd $ROOT
while read -r c p; do
  [ -z "$c" ] && continue
  echo "========== $c.$p =========="
  python3 harness/diffmsg.py harness/results/$c.$p.out harness/results/$c.$p.ours 2>&1 | grep -vE 'E1101|I1101' | grep -iE 'FP|FN|^ |samples|missing|extra|diff|code' | head -30
done <<'EOF'
pip full
sqlalchemy hook
sqlalchemy full
scikit-learn full
nova full
sympy full
pylfunc full
core full
EOF
