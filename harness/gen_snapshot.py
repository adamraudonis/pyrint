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
PARENT_FIX = []


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


def _empty_klass(node):
    """klass.__module__/__name__ + instance-branch flag mirroring
    manager.infer_ast_from_something — the Rust engine replays the
    `modastroid.igetattr(name, context)` lookup (with its counter bumps)
    instead of introspecting live objects."""
    if not node.has_underlying_object():
        return None
    obj = node.object
    try:
        if hasattr(obj, "__class__") and not isinstance(obj, type):
            klass, inst = obj.__class__, True
        elif isinstance(obj, type):
            klass, inst = obj, False
        else:
            return None
        return {"mod": klass.__module__, "name": klass.__name__, "inst": inst}
    except Exception:  # noqa: BLE001
        return None


def _empty_inf(node):
    """What this EmptyNode infers to (EmptyNode._infer ->
    infer_ast_from_something on the live object), as resolvable
    descriptors: the Rust engine cannot introspect live objects."""
    from astroid import bases as _bases, util as _util

    try:
        vals = list(node.infer())
    except Exception:  # noqa: BLE001
        return None
    out = []
    for v in vals[:4]:
        try:
            if isinstance(v, _util.UninferableBase):
                out.append({"t": "u"})
            elif isinstance(v, nodes.Const):
                out.append({"t": "const", "v": const_value(v.value)})
            elif isinstance(v, nodes.ClassDef):
                out.append({"t": "class", "q": v.qname()})
            elif isinstance(v, nodes.FunctionDef):
                out.append({"t": "func", "q": v.qname()})
            elif isinstance(v, _bases.Instance):
                out.append({"t": "inst", "q": v._proxied.qname()})
            else:
                out.append({"t": "u"})
        except Exception:  # noqa: BLE001
            out.append({"t": "u"})
    return out


def ser(node, pos_parent=None):
    global COUNTER
    if node is None:
        return None
    if isinstance(node, (list, tuple)):
        return [ser(n, pos_parent) for n in node]
    if id(node) in NODE_IDS:
        # the SAME astroid object reachable from several tree positions
        # (raw_building re-attaches: builtins.type in every exception's
        # __class__ locals, OSError==IOError body re-appends). Identity is
        # load-bearing (`cls != self` metaclass guards, set() dedup, lru
        # keys) — emit a reference, never a second copy. Distinct raw
        # builds that happen to be content-identical (sys.excepthook vs
        # sys.__excepthook__) have different id()s and stay separate.
        return {"k": "Ref", "r": NODE_IDS[id(node)]}
    d = {"k": type(node).__name__, "i": COUNTER}
    NODE_IDS[id(node)] = COUNTER
    COUNTER += 1
    # astroid's final .parent (add_local_node overwrites on every attach;
    # the last attach wins) can differ from this serialization position —
    # record the discrepancy for the loader's parent fixup.
    if pos_parent is not None and getattr(node, "parent", None) is not pos_parent:
        PARENT_FIX.append((d["i"], id(node.parent)))
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
    if isinstance(node, (nodes.ClassDef, nodes.FunctionDef)):
        # raw-built nodes are appended/reparented by add_local_node and the
        # same object can serialize at several tree positions; record the
        # authoritative runtime qname so the Rust side renders identically.
        try:
            d["qn"] = node.qname()
        except Exception:  # noqa: BLE001
            pass
    if type(node).__name__ == "EmptyNode":
        d["einf"] = _empty_inf(node)
        d["ek"] = _empty_klass(node)
    # children in _astroid_fields order
    ch = {}
    for field in node._astroid_fields:
        ch[field] = ser(getattr(node, field), node)
    if ch:
        d["ch"] = ch
    # scope locals (after children so ids exist)
    if hasattr(node, "locals") and isinstance(
        node, (nodes.Module, nodes.ClassDef, nodes.FunctionDef, nodes.Lambda)
    ):
        # Nodes reachable only through locals (brain-replaced str/bytes
        # methods, bootstrap set_local extras like 'generator') are
        # serialized into an "xtra" sidecar list so locals refs resolve.
        loc = {}
        xtra = []
        for name, vals in node.locals.items():
            ids = []
            for v in vals:
                if id(v) not in NODE_IDS:
                    xtra.append(ser(v, node))
                ids.append(NODE_IDS[id(v)])
            loc[name] = ids
        d["locals"] = loc
        if xtra:
            d["xtra"] = xtra
    if isinstance(node, nodes.ClassDef):
        ia = {}
        xtra_ia = []
        for name, vals in node.instance_attrs.items():
            ids = []
            for v in vals:
                if id(v) not in NODE_IDS:
                    xtra_ia.append(ser(v, node))
                ids.append(NODE_IDS[id(v)])
            ia[name] = ids
        d["iattrs"] = ia
        if xtra_ia:
            d["xtra_iattrs"] = xtra_ia
        d["basenames"] = list(node.basenames)
    return d


def snapshot_module(modname):
    global COUNTER, NODE_IDS, PARENT_FIX
    COUNTER = 0
    NODE_IDS = {}
    PARENT_FIX = []
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
    # parent fixups: serialization position != astroid's final .parent
    # (resolved here because the parent may serialize after the child)
    parfix = {}
    for i, pid in PARENT_FIX:
        if pid in NODE_IDS:
            parfix[str(i)] = NODE_IDS[pid]
    if parfix:
        data["parfix"] = parfix
    return data


def main():
    # Force bootstrap FIRST so that 'builtins' is the bootstrap module
    # (brain str/bytes method stubs, synthetic generator/async_generator
    # classes, NoneType/... extras). Without this, ast_from_module_name
    # ('builtins') as the very first astroid call builds an UNEXTENDED
    # duplicate (manager.module_build path) and we'd snapshot that.
    from astroid.builder import AstroidBuilder

    AstroidBuilder(MANAGER)  # triggers manager.bootstrap() once
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
