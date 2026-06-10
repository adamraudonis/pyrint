#!/usr/bin/env python3
"""Dump astroid inference results for differential testing of pyinfer.

Usage: dump_infer.py <corpus_root> <fileitems.jsonl> [--only path1,path2,...]

fileitems.jsonl: {"name": modname, "path": relpath} per line, in pylint's
exact order (generate with `prylint --dump-fileitems`). ALL files are
pre-built into astroid's cache first (mirroring pylint's phase 1), then the
--only subset (default: all) is dumped.

For every Name(Load)/Attribute(Load)/Call node, preorder:
  <line>:<col>:<Kind> -> v1 | v2 | ...
Value rendering (stable, port-friendly):
  U                              Uninferable
  ERR                            InferenceError
  Const:<type>:<repr[:40]>       Const nodes
  Class:<qname>                  ClassDef
  Func:<qname>                   FunctionDef (incl. type suffix)
  Lambda:<module>:<line>
  Module:<name>
  Inst:<qname>                   bases.Instance of class
  BM:<qname> / UM:<qname>        Bound/UnboundMethod
  Gen:<qname> / AGen:<qname>     Generator
  List:<n> Tuple:<n> Set:<n> Dict:<n>  container literals
  ExcInst:<qname> Super Prop:<qname> Partial:<qname> Union Frozen
  Slice DictItems DictKeys DictValues Unknown Empty
"""

import json
import os
import sys

import astroid
from astroid import MANAGER, bases, nodes, objects, util


def render(v):
    if isinstance(v, util.UninferableBase):
        return "U"
    if isinstance(v, nodes.Const):
        r = repr(v.value)
        if len(r) > 40:
            r = r[:40]
        return f"Const:{type(v.value).__name__}:{r}"
    if isinstance(v, nodes.ClassDef):
        return f"Class:{v.qname()}"
    if isinstance(v, objects.Property):
        return f"Prop:{v.qname()}"
    if isinstance(v, objects.PartialFunction):
        return f"Partial:{v.qname()}"
    if isinstance(v, nodes.FunctionDef):
        return f"Func:{v.qname()}"
    if isinstance(v, nodes.Lambda):
        return f"Lambda:{v.root().name}:{v.fromlineno}"
    if isinstance(v, nodes.Module):
        return f"Module:{v.name}"
    if isinstance(v, objects.ExceptionInstance):
        return f"ExcInst:{v._proxied.qname()}"
    if isinstance(v, objects.Super):
        return "Super"
    if isinstance(v, objects.FrozenSet):
        return "Frozen"
    if isinstance(v, objects.DictItems):
        return "DictItems"
    if isinstance(v, objects.DictKeys):
        return "DictKeys"
    if isinstance(v, objects.DictValues):
        return "DictValues"
    if isinstance(v, bases.BoundMethod):
        return f"BM:{v._proxied.qname()}"
    if isinstance(v, bases.UnboundMethod):
        return f"UM:{v._proxied.qname()}"
    if isinstance(v, bases.AsyncGenerator):
        return f"AGen:{v.parent.qname()}"
    if isinstance(v, bases.Generator):
        return f"Gen:{v.parent.qname()}"
    if isinstance(v, bases.UnionType):
        return "Union"
    if isinstance(v, nodes.List):
        return f"List:{len(v.elts)}"
    if isinstance(v, nodes.Tuple):
        return f"Tuple:{len(v.elts)}"
    if isinstance(v, nodes.Set):
        return f"Set:{len(v.elts)}"
    if isinstance(v, nodes.Dict):
        return f"Dict:{len(v.items)}"
    if isinstance(v, nodes.Slice):
        return "Slice"
    if isinstance(v, nodes.Unknown):
        return "Unknown"
    if isinstance(v, nodes.EmptyNode):
        return "Empty"
    if isinstance(v, bases.Instance):
        return f"Inst:{v._proxied.qname()}"
    return f"Other:{type(v).__name__}"


def infer_node(n):
    try:
        return [render(v) for v in n.infer()]
    except astroid.InferenceError:
        return ["ERR"]
    except RecursionError:
        return ["RECURSION"]
    except Exception as e:  # noqa: BLE001
        return [f"CRASH:{type(e).__name__}"]


def dump_file(tree, out):
    def walk(n):
        interesting = (
            (isinstance(n, nodes.Name))
            or (isinstance(n, nodes.Attribute))
            or isinstance(n, nodes.Call)
        )
        if interesting:
            vals = infer_node(n)
            out.append(
                f"{n.fromlineno}:{n.col_offset}:{type(n).__name__} -> {' | '.join(vals)}"
            )
        for c in n.get_children():
            walk(c)

    walk(tree)


def main():
    root = sys.argv[1]
    items_path = sys.argv[2]
    only = None
    for a in sys.argv[3:]:
        if a.startswith("--only="):
            only = set(a.split("=", 1)[1].split(","))
    os.chdir(root)
    sys.path.insert(0, os.path.realpath(root))
    items = [json.loads(l) for l in open(items_path) if l.strip()]
    # Phase 1 mirror: build ALL target files into the cache, in order.
    trees = {}
    for it in items:
        try:
            trees[it["path"]] = MANAGER.ast_from_file(it["path"], it["name"], source=True)
        except Exception:  # noqa: BLE001
            trees[it["path"]] = None
    # Phase 2: dump
    sys.setrecursionlimit(8000)
    for it in items:
        path = it["path"]
        if only is not None and path not in only:
            continue
        print(f"=== {path}")
        tree = trees.get(path)
        if tree is None:
            print("NOTREE")
            continue
        out = []
        try:
            dump_file(tree, out)
        except RecursionError:
            out.append("FILE_RECURSION")
        print("\n".join(out))


if __name__ == "__main__":
    main()
