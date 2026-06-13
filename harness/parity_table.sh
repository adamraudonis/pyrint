#!/bin/bash
# Consolidated Phase-F hook-profile parity table for the given corpora.
# Columns: full (footer-included bytecmp), body (footer-stripped + crash-norm),
# footer (rating-line match), exit (gt==ours), FP (ours-only lines), FN
# (GT-only lines). Excludes only module-HEADER lines from FP/FN.
set -u
ROOT=~/Desktop/Projects/prylint
CN='s#[^'\'' ]*pylint-crash-[0-9-]*\.txt#CRASH#g'
printf "%-14s %-5s %-5s %-6s %-10s %-6s %-6s\n" CORPUS full body footer exit FP FN
for c in "$@"; do
  o="$ROOT/harness/results/$c.hook.out"
  u="$ROOT/harness/results/$c.hook.ours"
  [ -f "$o" ] && [ -f "$u" ] || { printf "%-14s (missing)\n" "$c"; continue; }
  python3 "$ROOT/harness/strip_footer.py" "$o" | sed "$CN" > /tmp/g 2>/dev/null
  python3 "$ROOT/harness/strip_footer.py" "$u" | sed "$CN" > /tmp/o 2>/dev/null
  full=$(python3 "$ROOT/harness/bytecmp.py" "$o" "$u" && echo Y || echo N)
  body=$(python3 "$ROOT/harness/bytecmp.py" /tmp/g /tmp/o && echo Y || echo N)
  gtf=$(grep "rated at" "$o" | tail -1)
  ourf=$(grep "rated at" "$u" | tail -1)
  [ "$gtf" = "$ourf" ] && ft=Y || ft=N
  eg=$(cat "$ROOT/harness/results/$c.hook.exit")
  eo=$(cat "$ROOT/harness/results/$c.hook.ours.exit")
  [ "$eg" = "$eo" ] && ex="$eg==$eo" || ex="$eg!=$eo"
  fp=$(diff /tmp/g /tmp/o | grep '^>' | grep -vc "^\*\*\*\*")
  fn=$(diff /tmp/g /tmp/o | grep '^<' | grep -vc "^\*\*\*\*")
  printf "%-14s %-5s %-5s %-6s %-10s %-6s %-6s\n" "$c" "$full" "$body" "$ft" "$ex" "$fp" "$fn"
done
