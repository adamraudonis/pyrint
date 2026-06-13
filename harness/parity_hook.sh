#!/bin/bash
# Phase-F hook-profile parity table: run ours, compare against the isolated GT
# (footer INCLUDED) + exit code. Body = footer-stripped comparison; FP = lines
# ours emits that GT does not (excluding module headers) — the cardinal sin.
set -u
ROOT=~/Desktop/Projects/prylint
CORPORA="$*"
for c in $CORPORA; do
  [ -f "$ROOT/harness/results/$c.hook.out" ] || continue
  bash "$ROOT/harness/run_full.sh" "$c" hook >/dev/null 2>&1
  exitgt=$(cat "$ROOT/harness/results/$c.hook.exit")
  exitours=$(cat "$ROOT/harness/results/$c.hook.ours.exit")
  # normalize the unreproducible F0002 crash-template path (same class as
  # bytecmp.py) before the line diff so it isn't miscounted as FP/FN.
  CN='s#[^\x27 ]*pylint-crash-[0-9-]*\.txt#CRASH#g'
  python3 "$ROOT/harness/strip_footer.py" "$ROOT/harness/results/$c.hook.out" | sed "$CN" > /tmp/p_gt_b 2>/dev/null
  python3 "$ROOT/harness/strip_footer.py" "$ROOT/harness/results/$c.hook.ours" | sed "$CN" > /tmp/p_ours_b 2>/dev/null
  full=$(python3 "$ROOT/harness/bytecmp.py" "$ROOT/harness/results/$c.hook.out" "$ROOT/harness/results/$c.hook.ours" && echo Y || echo N)
  body=$(python3 "$ROOT/harness/bytecmp.py" /tmp/p_gt_b /tmp/p_ours_b && echo Y || echo N)
  fp=$(diff /tmp/p_gt_b /tmp/p_ours_b | grep '^>' | grep -vc "^\*\*\*\*")
  fn=$(diff /tmp/p_gt_b /tmp/p_ours_b | grep '^<' | grep -vc "^\*\*\*\*")
  printf "%-14s full=%s body=%s exit(gt=%s ours=%s) FP=%s FN=%s\n" "$c" "$full" "$body" "$exitgt" "$exitours" "$fp" "$fn"
done
