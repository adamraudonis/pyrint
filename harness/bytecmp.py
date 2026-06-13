#!/usr/bin/env python3
"""Byte comparison with F0002 crash-template-path normalization.

Usage: bytecmp.py <a> <b>   -- exit 0 iff equal, 1 if different (like cmp -s)

pylint's F0002 (astroid-error) message embeds a crash-template path
  <PYLINT_HOME>/pylint-crash-%Y-%m-%d-%H-%M-%S.txt
that is INHERENTLY unreproducible across runs: (1) the %Y-..-%S timestamp is
wall-clock, and (2) the PYLINT_HOME prefix depends on the (isolated) cache dir
each run was given AND on macOS realpath (/tmp vs /private/tmp). Two pylint
runs never agree on it either. Both inputs are compared after collapsing the
WHOLE crash-template path to a fixed sentinel; everything else (including any
real, reproducible PYLINT_HOME path elsewhere in the output) stays raw-byte
compared.
"""

import re
import sys

# Collapse "<anydir>/pylint-crash-<ts>.txt" (the only place the path appears).
_CRASH = re.compile(rb"[^'\s]*pylint-crash-[0-9-]*\.txt")


def norm(data: bytes) -> bytes:
    return _CRASH.sub(b"CRASH-TEMPLATE", data)


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__, file=sys.stderr)
        return 2
    with open(sys.argv[1], "rb") as fa, open(sys.argv[2], "rb") as fb:
        a, b = fa.read(), fb.read()
    return 0 if norm(a) == norm(b) else 1


if __name__ == "__main__":
    sys.exit(main())
