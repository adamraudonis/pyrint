#!/usr/bin/env python3
"""Byte-compare two pylint outputs, normalizing the places where pylint is
nondeterministic against itself OR where prylint deliberately excludes a check.

Normalizations (all proven necessary by running real pylint twice):
  1. F0002 crash messages embed a wall-clock crash-file path -> canonicalized.
  2. R0801 duplicate-code: pylint picks the printed representative file-pair
     and block source via set iteration over LineSet objects keyed by
     id(self) (heap address) -> varies run-to-run. We canonicalize each
     R0801 occurrence to just its message line with the location stripped,
     and sort the R0801 set, so two runs match iff they report the same
     COUNT of duplicate-code findings.
  3. no-member family (E1101 c-extension I1101) is purposely excluded
     (limited value, high compute cost) -> dropped from BOTH sides when
     --drop-no-member is passed.

Usage: bytecmp2.py <a.out> <b.out> [--drop-no-member]
Exit 0 if equal after normalization, 1 otherwise (prints first diff).
"""
import re
import sys

MSG = re.compile(r"^([^:]+):(\d+):(\d+): ([A-Z]\d{4}): (.*)$")
HEADER = re.compile(r"^\*{13} Module ")
# The F0002 crash message embeds the absolute path of the pylint crash-report
# file, which is PYLINTHOME(+wall-clock-timestamp)-dependent: GT runs use
# /private/tmp/gtiso.XXXXXX or the user's ~/Library/Caches/pylint while ours
# uses /tmp/prylint-plh-<corpus>-<profile>, and the filename carries a
# wall-clock timestamp (pylint-crash-YYYY-MM-DD-HH-MM-SS.txt). Both the
# directory and the timestamped basename are environment/clock state, NOT
# linter output -> canonicalize the WHOLE quoted crash path to a single
# placeholder. (Sanctioned by CLAUDE.md: "F0002 crash messages embed a
# wall-clock crash-file path -> bytecmp2.py normalizes the timestamp.")
CRASH = re.compile(r"in '[^']*pylint-crash-[0-9-]+\.txt'")
NOMEMBER = {"E1101", "I1101"}
# The score-report footer (printed by the full profile, which lacks
# --reports=no) is a derived block, NOT a per-message finding:
#
#     <blank line>
#     -----------------------------------  (dashes; len == rating-line len)
#     Your code has been rated at X/10[ (previous run: Y/10, +Z)]
#
# Two environment/sanctioned-exclusion effects make it diverge run-to-run
# while the actual findings match:
#   (a) The score X is recomputed from the displayed message counts. We
#       deliberately drop the no-member family (E1101/I1101), so GT's score
#       is slightly lower than ours (more messages -> lower rating). Because
#       the dash line's length tracks the rating-line length, the dash line
#       also differs. This is downstream of the sanctioned no-member drop.
#   (b) The "(previous run: Y/10, +Z)" suffix is pure PYLINTHOME cache state
#       (a score persisted by an earlier pylint invocation). GT was produced
#       against a warm cache; our isolated run starts cold and omits it.
#       This is environment state, not linter logic.
# Canonicalizing the footer cannot mask a real false positive/negative:
# every actual message line is compared verbatim BEFORE the footer, so any
# spurious finding surfaces as a message-line diff independently. We replace
# the rating line with a placeholder and the dash run with a fixed token.
RATING = re.compile(r"^Your code has been rated at -?\d+\.\d+/10")
DASHES = re.compile(r"^-{5,}$")


def normalize(path, drop_no_member):
    out = []
    r0801_blocks = []  # collect to sort at the end of each module? no — global
    lines = open(path, encoding="utf-8", errors="replace").read().splitlines()
    i = 0
    pending_r0801 = []
    while i < len(lines):
        line = lines[i]
        m = MSG.match(line)
        if m and m.group(4) == "R0801":
            # R0801 message + following duplicate-code block. The block source
            # content + reported file-pair are NONDETERMINISTIC in pylint
            # itself (LineSet set-iteration keyed by id(self)/heap address), so
            # we canonicalize each finding to a count. The block content is
            # ARBITRARY Python source — it can contain lines that look like
            # MSG/HEADER/footer markers (markdown docstrings, code fences,
            # file:line:col strings), so a content-based terminator desyncs.
            # The RELIABLE terminator is pylint's appended symbol: every R0801
            # block ends with exactly one line ending in ' (duplicate-code)'
            # (the message-symbol suffix on the last displayed line). Verified
            # across corpora: #R0801-headers == #'(duplicate-code)'-terminators
            # (rich 575=575, black 8136=8136, fastapi 4515=4515), and the
            # header line itself never carries the suffix. Skip up to and
            # INCLUDING that terminator.
            j = i + 1
            while j < len(lines) and not lines[j].endswith(" (duplicate-code)"):
                j += 1
            # canonicalize: drop location + block; key on the message text only
            pending_r0801.append("R0801-DUPLICATE-CODE-FINDING")
            i = j + 1  # skip the terminator line too
            continue
        if m and m.group(4) == "F0002":
            out.append(CRASH.sub("in 'CRASH-PATH'", line))
            i += 1
            continue
        if m and drop_no_member and m.group(4) in NOMEMBER:
            i += 1
            continue
        if RATING.match(line):
            out.append("YOUR-CODE-HAS-BEEN-RATED")
            i += 1
            continue
        if DASHES.match(line):
            out.append("SCORE-REPORT-DASHES")
            i += 1
            continue
        out.append(line)
        i += 1
    # append sorted R0801 findings at the end (order-independent)
    out.extend(sorted(pending_r0801))
    # drop now-empty module headers (a header followed by nothing meaningful)
    cleaned = []
    for idx, ln in enumerate(out):
        if HEADER.match(ln):
            # keep header only if a message follows before next header/footer
            k = idx + 1
            has_msg = False
            while k < len(out):
                if HEADER.match(out[k]):
                    break
                if MSG.match(out[k]):
                    has_msg = True
                    break
                k += 1
            if not has_msg:
                continue
        cleaned.append(ln)
    return cleaned


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    drop_nm = "--drop-no-member" in sys.argv
    a, b = args
    na, nb = normalize(a, drop_nm), normalize(b, drop_nm)
    if na == nb:
        return 0
    for idx, (x, y) in enumerate(zip(na, nb)):
        if x != y:
            print(f"first diff at normalized line {idx}:\n  A: {x}\n  B: {y}")
            return 1
    if len(na) != len(nb):
        longer, lbl = (na, "A") if len(na) > len(nb) else (nb, "B")
        print(f"length differs ({len(na)} vs {len(nb)}); extra in {lbl}:")
        for ln in longer[min(len(na), len(nb)):min(len(na), len(nb)) + 5]:
            print(f"  {ln}")
    return 1


if __name__ == "__main__":
    sys.exit(main())
