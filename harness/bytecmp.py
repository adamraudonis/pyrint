#!/usr/bin/env python3
"""Byte comparison with F0002 crash-file timestamp normalization.

Usage: bytecmp.py <a> <b>   -- exit 0 iff equal, 1 if different (like cmp -s)

pylint's F0002 (astroid-error) message embeds a wall-clock crash-template
path: <PYLINT_HOME>/pylint-crash-%Y-%m-%d-%H-%M-%S.txt — unreproducible even
between two pylint runs. Both inputs are compared after rewriting
  pylint-crash-[0-9-]*\\.txt  ->  pylint-crash-TS.txt
Everything else (including the PYLINT_HOME prefix) stays raw-byte compared.
"""

import re
import sys


def norm(data: bytes) -> bytes:
    return re.sub(rb"pylint-crash-[0-9-]*\.txt", b"pylint-crash-TS.txt", data)


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__, file=sys.stderr)
        return 2
    with open(sys.argv[1], "rb") as fa, open(sys.argv[2], "rb") as fb:
        a, b = fa.read(), fb.read()
    return 0 if norm(a) == norm(b) else 1


if __name__ == "__main__":
    sys.exit(main())
