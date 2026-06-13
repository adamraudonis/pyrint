#!/bin/bash
# Phase-F full-profile parity: run ours, compare vs isolated GT (footer +
# exit). NOTE: full-mode pylint is NONDETERMINISTIC in its R0801 duplicate-code
# block ordering (verified: two isolated runs differ), so full byte parity is
# only achievable on corpora without R0801 emissions; the FP/FN split isolates
# the deterministic message body from the R0801 / pre-existing checker gaps.
set -u
ROOT=~/Desktop/Projects/prylint
for c in $*; do
  [ -f "$ROOT/harness/results/$c.full.out" ] || continue
  bash "$ROOT/harness/run_full.sh" "$c" full >/dev/null 2>&1
  exitgt=$(cat "$ROOT/harness/results/$c.full.exit")
  exitours=$(cat "$ROOT/harness/results/$c.full.ours.exit")
  python3 "$ROOT/harness/strip_footer.py" "$ROOT/harness/results/$c.full.out" > /tmp/pf_gt_b 2>/dev/null
  python3 "$ROOT/harness/strip_footer.py" "$ROOT/harness/results/$c.full.ours" > /tmp/pf_ours_b 2>/dev/null
  full=$(python3 "$ROOT/harness/bytecmp.py" "$ROOT/harness/results/$c.full.out" "$ROOT/harness/results/$c.full.ours" && echo Y || echo N)
  body=$(python3 "$ROOT/harness/bytecmp.py" /tmp/pf_gt_b /tmp/pf_ours_b && echo Y || echo N)
  fp=$(diff /tmp/pf_gt_b /tmp/pf_ours_b | grep '^>' | grep -vc "^\*\*\*\*")
  fn=$(diff /tmp/pf_gt_b /tmp/pf_ours_b | grep '^<' | grep -vc "^\*\*\*\*")
  r0801=$(grep -c "R0801\|duplicate-code" "$ROOT/harness/results/$c.full.out")
  printf "%-14s full=%s body=%s exit(gt=%s ours=%s) FP=%s FN=%s R0801gt=%s\n" "$c" "$full" "$body" "$exitgt" "$exitours" "$fp" "$fn" "$r0801"
done
