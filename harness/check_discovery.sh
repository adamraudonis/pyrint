#!/bin/bash
ROOT=~/Desktop/Projects/prylint
for c in "$@"; do
  cd $ROOT/corpora/$c || continue
  $ROOT/target/release/prylint . --dump-fileitems | python3 -c "import sys,json; [print(json.dumps(json.loads(l))) for l in sys.stdin]" > /tmp/rust_$c.txt
  $ROOT/.venv-pylint/bin/python $ROOT/harness/dump_fileitems.py . 2>/dev/null | python3 -c "import sys,json; [print(json.dumps(json.loads(l))) for l in sys.stdin]" > /tmp/py_$c.txt
  if diff -q /tmp/rust_$c.txt /tmp/py_$c.txt > /dev/null; then
    echo "$c: IDENTICAL ($(wc -l < /tmp/rust_$c.txt) items)"
  else
    echo "$c: DIFFERS rust=$(wc -l < /tmp/rust_$c.txt) py=$(wc -l < /tmp/py_$c.txt)"
    diff /tmp/rust_$c.txt /tmp/py_$c.txt | head -6
  fi
done
