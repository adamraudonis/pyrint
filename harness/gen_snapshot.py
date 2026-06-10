#!/usr/bin/env python3
"""Serialize astroid's post-brain view of `builtins` and all C-extension
stdlib modules to JSON for the Rust inference engine.

These modules have no source on disk: astroid fabricates them via
raw_building (live introspection) + brain transforms. Freezing the pinned
venv's view guarantees exactness.

Output: crates/pyinfer/snapshot/<module>.json
Schema per node:
  {"k": <astroid class name>, "pos": [line, col, end_line, end_col] | null,
   ...kind-specific fields...,
   "ch": {field_name: <node|[node]|null|...>},   # _astroid_fields, in order
   "locals": {name: [<idx into nodes of this doc>]},   # scope nodes only
   "iattrs": {name: [...]},                            # classes only
   }
Nodes are inlined depth-first (the tree IS the JSON). Cross-references are
not needed: locals reference nodes by a preorder index assigned during
serialization ("i" field on every node).
"""

import json
import os
import sys

import astroid
from astroid import MANAGER, nodes

OUT_DIR = os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
    "crates", "pyinfer", "snapshot",
)

COUNTER = 0
NODE_IDS = {}


def const_value(v):
    if v is None or v is Ellipsis:
        return {"t": "none" if v is None else "ellipsis"}
    if isinstance(v, bool):
        return {"t": "bool", "v": v}
    if isinstance(v, int):
        return {"t": "int", "v": str(v)}
    if isinstance(v, float):
        return {"t": "float", "v": repr(v)}
    if isinstance(v, complex):
        return {"t": "complex", "re": repr(v.real), "im": repr(v.imag)}
    if isinstance(v, str):
        return {"t": "str", "v": v}
    if isinstance(v, bytes):
        return {"t": "bytes", "v": list(v)}
    if isinstance(v, frozenset):
        return {"t": "frozenset"}
    if isinstance(v, tuple):
        return {"t": "tuple"}
    return {"t": "other", "repr": repr(v)[:200]}


def ser(node):
    global COUNTER
    if node is None:
        return None
    if isinstance(node, (list, tuple)):
        return [ser(n) for n in node]
    d = {"k": type(node).__name__, "i": COUNTER}
    NODE_IDS[id(node)] = COUNTER
    COUNTER += 1
    if getattr(node, "lineno", None) is not None:
        d["pos"] = [node.lineno, getattr(node, "col_offset", 0) or 0]
    # kind-specific scalar fields
    for attr in ("name", "attrname", "op", "arg", "level", "modname", "type",
                 "vararg", "kwarg", "conversion"):
        if hasattr(node, attr):
            v = getattr(node, attr)
            if isinstance(v, (str, int, type(None))):
                d[attr] = v
    if isinstance(node, nodes.Const):
        d["value"] = const_value(node.value)
    if isinstance(node, (nodes.Import, nodes.ImportFrom)):
        d["names"] = [[n, a] for n, a in node.names]
    if isinstance(node, (nodes.Global, nodes.Nonlocal)):
        d["gnames"] = list(node.names)
    if isinstance(node, nodes.FunctionDef):
        d["ftype"] = node.type  # method/function/classmethod/staticmethod
    # children in _astroid_fields order
    ch = {}
    for field in node._astroid_fields:
        ch[field] = ser(getattr(node, field))
    if ch:
        d["ch"] = ch
    # scope locals (after children so ids exist)
    if hasattr(node, "locals") and isinstance(
        node, (nodes.Module, nodes.ClassDef, nodes.FunctionDef, nodes.Lambda)
    ):
        loc = {}
        for name, vals in node.locals.items():
            ids = [NODE_IDS[id(v)] for v in vals if id(v) in NODE_IDS]
            loc[name] = ids
        d["locals"] = loc
    if isinstance(node, nodes.ClassDef):
        ia = {}
        for name, vals in node.instance_attrs.items():
            ids = [NODE_IDS[id(v)] for v in vals if id(v) in NODE_IDS]
            ia[name] = ids
        d["iattrs"] = ia
        d["basenames"] = list(node.basenames)
    return d


def snapshot_module(modname):
    global COUNTER, NODE_IDS
    COUNTER = 0
    NODE_IDS = {}
    try:
        mod = MANAGER.ast_from_module_name(modname)
    except Exception as e:  # noqa: BLE001
        print(f"  SKIP {modname}: {type(e).__name__}", file=sys.stderr)
        return None
    if mod.path and mod.file and mod.file.endswith((".py", ".pyi")):
        # pure-python: Rust parses the real source; no snapshot needed
        return "source"
    data = ser(mod)
    data["modname"] = modname
    data["pure_python"] = bool(mod.pure_python)
    return data


def main():
    os.makedirs(OUT_DIR, exist_ok=True)
    mods = ["builtins"]
    mods += [m for m in sys.builtin_module_names if not m.startswith("_") or True]
    # binary stdlib extension modules (lib-dynload)
    import sysconfig

    dynload = os.path.join(sysconfig.get_paths()["stdlib"], "lib-dynload")
    if os.path.isdir(dynload):
        for f in sorted(os.listdir(dynload)):
            if f.endswith(".so"):
                mods.append(f.split(".")[0])
    seen = set()
    manifest = {}
    for m in mods:
        if m in seen:
            continue
        seen.add(m)
        result = snapshot_module(m)
        if result == "source" or result is None:
            continue
        path = os.path.join(OUT_DIR, f"{m}.json")
        with open(path, "w", encoding="utf-8") as f:
            json.dump(result, f, separators=(",", ":"))
        manifest[m] = os.path.getsize(path)
        print(f"  {m}: {manifest[m]} bytes")
    with open(os.path.join(OUT_DIR, "MANIFEST.json"), "w", encoding="utf-8") as f:
        json.dump(sorted(manifest), f)
    print(f"{len(manifest)} modules snapshotted to {OUT_DIR}")


if __name__ == "__main__":
    main()
