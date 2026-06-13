"""Stdlib-only PYLINT_HOME stats-pickle helper (Phase F).

prylint shells out to this script (python3 -I) for the two persistent-stats
operations pylint performs in full mode (notes/09-pipeline-noE.md §5):

  load  <path>                  -> read previous_stats.global_note for the
                                   "(previous run: X.XX/10, +Y.YY)" footer
                                   suffix. Mirrors caching.load_results
                                   (caching.py:30-49): missing file or ANY
                                   exception -> None (silent tolerance).
  save  <path> <state-json>     -> write a pickle that real pylint's
                                   caching.load_results can read back: a
                                   genuine pylint.utils.linterstats.LinterStats
                                   instance with the given __dict__. We DO NOT
                                   import pylint (stdlib-only): the protocol-4
                                   opcodes (STACK_GLOBAL + NEWOBJ + BUILD) name
                                   the class by module/qualname, and the class
                                   is resolved in pylint's OWN process at load.

Protocol: one request per stdin LINE (JSON), one response per stdout line
(JSON), matching the oracle coprocess pattern so the Rust side can keep a
persistent interpreter.

  request  {"op":"load","path":"..."} ->
           {"global_note": <float|null>}     (null on missing/corrupt)
  request  {"op":"save","path":"...","state":{...}} ->
           {"ok": true}  or  {"ok": false, "err": "..."}

The "state" dict maps LinterStats attribute names to JSON values. Sets are
encoded as {"__set__": [...]} so modules_names/dependencies round-trip; all
other containers are plain JSON dicts/lists. Ints stay ints, floats floats.
"""

import io
import json
import os
import pickle
import struct
import sys


# ---- load: read global_note without importing pylint --------------------

class _StubStats:
    """Stand-in for any GLOBAL the cache references; we only read __dict__."""

    def __setstate__(self, state):  # not used by 4.0.5 (plain BUILD)
        self.__dict__.update(state)


class _StubUnpickler(pickle.Unpickler):
    def find_class(self, module, name):
        # Resolve EVERY foreign class to the stub so load never needs pylint
        # or astroid importable. Builtins still resolve normally.
        if module in ("builtins", "__builtin__"):
            return super().find_class(module, name)
        return _StubStats


def _decode_set(obj):
    if isinstance(obj, dict) and set(obj.keys()) == {"__set__"}:
        return set(obj["__set__"])
    return obj


def op_load(path):
    try:
        if not os.path.exists(path):
            return {"global_note": None}
        with open(path, "rb") as f:
            obj = _StubUnpickler(f).load()
        note = getattr(obj, "global_note", None)
        if isinstance(note, bool):  # bool is an int subclass; keep as-is
            note = int(note)
        if note is None or isinstance(note, (int, float)):
            return {"global_note": note}
        return {"global_note": None}
    except Exception:  # caching.load_results swallows everything -> None
        return {"global_note": None}


# ---- save: emit a real LinterStats pickle via raw protocol-4 opcodes -----

def _short_bu(s):
    b = s.encode("utf-8")
    if len(b) < 256:
        return bytes([0x8C, len(b)]) + b
    return b"\x8d" + struct.pack("<I", len(b)) + b  # BINUNICODE8? use BINUNICODE


def _to_py(value):
    """Recursively decode the JSON wire form into real python objects."""
    if isinstance(value, dict):
        if set(value.keys()) == {"__set__"}:
            return set(_to_py(x) for x in value["__set__"])
        return {k: _to_py(v) for k, v in value.items()}
    if isinstance(value, list):
        return [_to_py(x) for x in value]
    return value


def op_save(path, state_wire):
    try:
        state = {k: _to_py(v) for k, v in state_wire.items()}
        body = bytearray()
        body += _short_bu("pylint.utils.linterstats")
        body += _short_bu("LinterStats")
        body += b"\x93"  # STACK_GLOBAL
        body += b")"     # EMPTY_TUPLE (NEWOBJ args)
        body += b"\x81"  # NEWOBJ -> cls.__new__(cls)
        sub = pickle.dumps(state, protocol=4)
        # strip PROTO(2) + optional FRAME(9) prefix and trailing STOP(1)
        assert sub[:2] == b"\x80\x04"
        i = 2
        if len(sub) > 2 and sub[2] == 0x95:
            i = 2 + 9
        assert sub[-1:] == b"."
        body += sub[i:-1]
        body += b"b"     # BUILD (apply state dict to the instance)
        out = bytearray()
        out += b"\x80\x04"
        out += b"\x95" + struct.pack("<Q", len(body) + 1)
        out += body
        out += b"."
        # caching.save_results does mkdir -p PYLINT_HOME first
        d = os.path.dirname(path)
        if d:
            os.makedirs(d, exist_ok=True)
        with open(path, "wb") as f:
            f.write(bytes(out))
        return {"ok": True}
    except Exception as ex:  # save_results notes the failure on stderr, continues
        return {"ok": False, "err": str(ex)}


def main():
    out = sys.stdout
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
            op = req.get("op")
            if op == "load":
                resp = op_load(req["path"])
            elif op == "save":
                resp = op_save(req["path"], req.get("state", {}))
            else:
                resp = {"err": "unknown op"}
        except Exception as ex:
            resp = {"err": str(ex)}
        out.write(json.dumps(resp) + "\n")
        out.flush()


if __name__ == "__main__":
    main()
