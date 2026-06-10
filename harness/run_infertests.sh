#!/bin/bash
# Run every probe in harness/infertests/ through both the astroid oracle and
# prylint --dump-infer; report PASS/FAIL per probe. Usage: run_infertests.sh [name]
ROOT=~/Desktop/Projects/prylint
TESTS=$ROOT/harness/infertests
PY=$ROOT/.venv-pylint/bin/python
fail=0
for f in "$TESTS"/${1:-*}.py; do
  [ -e "$f" ] || continue
  name=$(basename "$f" .py)
  dir=$(mktemp -d)
  cp "$f" "$dir/probe.py"
  echo '{"name":"probe","path":"probe.py"}' > "$dir/items.jsonl"
  "$PY" $ROOT/harness/dump_infer.py "$dir" "$dir/items.jsonl" > "$dir/gt.out" 2>/dev/null
  (cd "$dir" && $ROOT/target/release/prylint . --dump-infer items.jsonl > ours.out 2>/dev/null)
  if diff -q "$dir/gt.out" "$dir/ours.out" > /dev/null 2>&1; then
    echo "PASS $name"
  else
    echo "FAIL $name"
    diff "$dir/gt.out" "$dir/ours.out" | head -12
    fail=1
  fi
  rm -rf "$dir"
done
exit $fail
