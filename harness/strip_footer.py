#!/usr/bin/env python3
"""Strip the pylint score footer from a ground-truth capture.

Until prylint implements the report/score footer (phase F), full-mode ground
truth is compared with the trailing footer removed. The footer is exactly
(notes/09-pipeline-noE.md §4.3):

    "\n" + "-" * len(msg) + "\n" + msg + "\n" + "\n"

where msg starts with "Your code has been rated at " (possibly with a
"(previous run: ...)" suffix) or "An exception occurred while rating: ...".
This is the ONLY sanctioned ground-truth transform.

Usage: strip_footer.py <file> [out]   (out defaults to stdout)
"""

import sys


def strip_footer(data: bytes) -> bytes:
    lines = data.split(b"\n")
    # trailing structure: [..., "", dashes, msg, "", ""] after split
    # (file ends with "msg\n\n" -> last two elements are b"" b"")
    if len(lines) >= 5 and lines[-1] == b"" and lines[-2] == b"":
        msg = lines[-3]
        dashes = lines[-4]
        blank = lines[-5]
        if (
            blank == b""
            and dashes
            and dashes == b"-" * len(dashes)
            and (
                msg.startswith(b"Your code has been rated at ")
                or msg.startswith(b"An exception occurred while rating:")
            )
            and len(dashes) == len(msg.decode("utf-8", "replace"))
        ):
            return b"\n".join(lines[:-5]) + (b"\n" if len(lines) > 5 else b"")
    return data


def main() -> None:
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    with open(sys.argv[1], "rb") as f:
        data = f.read()
    out = strip_footer(data)
    if len(sys.argv) > 2:
        with open(sys.argv[2], "wb") as f:
            f.write(out)
    else:
        sys.stdout.buffer.write(out)


if __name__ == "__main__":
    main()
