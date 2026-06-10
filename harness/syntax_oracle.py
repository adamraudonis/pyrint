#!/usr/bin/env python3
"""Exact-E0001 oracle: replicate pylint's get_ast() error paths for files the
Rust parser rejects.

Protocol: JSON lines on stdin: {"path": ..., "modname": ...}
          JSON lines on stdout, one of:
  {"ok": true}                                  -- astroid parses fine (caller bug or ruff/CPython mismatch)
  {"ok": true, "tokenize": {"line": N, "col": N, "msg": "..."}}
      -- astroid parses, but pylint's utils.tokenize_module would raise
         tokenize.TokenError (the tokenize-form E0001: args[0] verbatim,
         line/col from args[1]; pylinter.py:1080-1090)
  {"kind": "syntax-error", "line": N, "offset": N|null, "msg": "..."}
      -- msg is the full E0001 args: "Parsing failed: '<...>'"
  {"kind": "parse-error", "msg": "..."}          -- F0010 args
  {"kind": "astroid-error"}                      -- F0002 path

Mirrors pylint/lint/pylinter.py get_ast() with MANAGER.ast_from_file.
"""

import json
import sys
import tokenize

import astroid
from astroid import MANAGER


def handle(path, modname):
    try:
        module = MANAGER.ast_from_file(path, modname, source=True)
    except astroid.AstroidSyntaxError as ex:
        line = getattr(ex.error, "lineno", None)
        if line is None:
            line = 0
        offset = getattr(ex.error, "offset", None)
        return {
            "kind": "syntax-error",
            "line": line,
            "offset": offset,
            "msg": f"Parsing failed: '{ex.error}'",
        }
    except astroid.AstroidBuildingError as ex:
        return {"kind": "parse-error", "msg": str(ex)}
    except Exception:  # noqa: BLE001
        return {"kind": "astroid-error"}
    # pylint utils.tokenize_module (pylint/utils/utils.py:151-154)
    try:
        with module.stream() as stream:
            list(tokenize.tokenize(stream.readline))
    except tokenize.TokenError as ex:
        return {
            "ok": True,
            "tokenize": {
                "line": ex.args[1][0],
                "col": ex.args[1][1],
                "msg": ex.args[0],
            },
        }
    except Exception:  # noqa: BLE001
        pass  # other tokenize crashes take pylint's F0002 path; out of scope
    return {"ok": True}


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        req = json.loads(line)
        res = handle(req["path"], req["modname"])
        sys.stdout.write(json.dumps(res) + "\n")
        sys.stdout.flush()


if __name__ == "__main__":
    main()
