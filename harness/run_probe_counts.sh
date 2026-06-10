#!/bin/bash
# Usage: run_probe_counts.sh <probe-dir>  — diff counted dumps (oracle vs ours)
ROOT=~/Desktop/Projects/prylint
D="$1"
PY=$ROOT/.venv-pylint/bin/python
[ -f "$D/items.jsonl" ] || echo '{"name":"probe","path":"probe.py"}' > "$D/items.jsonl"
"$PY" $ROOT/harness/dump_infer_count.py "$D" "$D/items.jsonl" > "$D/gt_cnt.out" 2>/dev/null
(cd "$D" && PRYLINT_DUMP_COUNTS=1 $ROOT/target/release/prylint . --dump-infer items.jsonl > ours_cnt.out 2>/dev/null)
diff "$D/gt_cnt.out" "$D/ours_cnt.out" && echo COUNTS-MATCH
