#!/usr/bin/env python3
"""Dump astroid's view of Python files in a stable text format for differential
testing of the Rust pyast port.

Usage: dump_ast.py FILE [FILE...]
       dump_ast.py --modname NAME FILE

Each node, preorder via get_children():
  <depth*2 spaces><Kind> <fromlineno>:<col_offset>-<tolineno>(<end_lineno>):<end_col_offset> [extra]
Scope nodes append locals=[name,...] in insertion order.
"""

import sys

import astroid
from astroid import nodes


def fmt_pos(n):
    def s(v):
        return "-" if v is None else str(v)

    return f"{s(n.fromlineno)}:{s(n.col_offset)}-{s(n.tolineno)}({s(n.end_lineno)}):{s(n.end_col_offset)}"


def extra(n):
    parts = []
    if isinstance(n, (nodes.Name, nodes.AssignName, nodes.DelName)):
        parts.append(f"name={n.name}")
    elif isinstance(n, (nodes.FunctionDef, nodes.ClassDef)):
        parts.append(f"name={n.name}")
        if n.doc_node is not None:
            parts.append(f"doc@{n.doc_node.fromlineno}")
    elif isinstance(n, (nodes.Attribute, nodes.AssignAttr, nodes.DelAttr)):
        parts.append(f"attr={n.attrname}")
    elif isinstance(n, nodes.Const):
        v = n.value
        t = type(v).__name__
        if isinstance(v, str):
            parts.append(f"const=str:{len(v)}:{v[:20]!r}")
        elif isinstance(v, bytes):
            parts.append(f"const=bytes:{len(v)}")
        elif isinstance(v, bool):
            parts.append(f"const=bool:{v}")
        elif isinstance(v, int):
            parts.append(f"const=int:{v if abs(v) < 10**15 else 'big'}")
        elif isinstance(v, float):
            parts.append(f"const=float:{v!r}")
        else:
            parts.append(f"const={t}")
    elif isinstance(n, nodes.Import):
        parts.append(f"names={n.names!r}")
    elif isinstance(n, nodes.ImportFrom):
        parts.append(f"from={n.modname}:{n.level!r}:{n.names!r}")
    elif isinstance(n, nodes.BinOp):
        parts.append(f"op={n.op}")
    elif isinstance(n, nodes.BoolOp):
        parts.append(f"op={n.op}")
    elif isinstance(n, nodes.UnaryOp):
        parts.append(f"op={n.op}")
    elif isinstance(n, nodes.AugAssign):
        parts.append(f"op={n.op}")
    elif isinstance(n, nodes.Compare):
        parts.append(f"ops={[o[0] for o in n.ops]!r}")
    elif isinstance(n, nodes.Keyword):
        parts.append(f"arg={n.arg!r}")
    elif isinstance(n, nodes.Slice):
        pass
    elif isinstance(n, nodes.Arguments):
        parts.append(f"vararg={n.vararg!r} kwarg={n.kwarg!r}")
    elif isinstance(n, nodes.Global):
        parts.append(f"names={n.names!r}")
    elif isinstance(n, nodes.Nonlocal):
        parts.append(f"names={n.names!r}")
    if isinstance(n, nodes.Module):
        parts.append(f"name={n.name} package={n.package}")
        if n.doc_node is not None:
            parts.append(f"doc@{n.doc_node.fromlineno}")
    if hasattr(n, "locals") and isinstance(
        n, (nodes.Module, nodes.FunctionDef, nodes.ClassDef, nodes.Lambda, nodes.GeneratorExp,
            nodes.ListComp, nodes.SetComp, nodes.DictComp)
    ):
        parts.append("locals=[" + ",".join(n.locals.keys()) + "]")
    return " ".join(parts)


def dump(n, depth, out):
    kind = type(n).__name__
    line = f"{'  ' * depth}{kind} {fmt_pos(n)}"
    e = extra(n)
    if e:
        line += " " + e
    out.append(line)
    for c in n.get_children():
        dump(c, depth + 1, out)


def main():
    args = sys.argv[1:]
    modname = "mod"
    if args and args[0] == "--modname":
        modname = args[1]
        args = args[2:]
    for path in args:
        print(f"=== {path}")
        try:
            tree = astroid.MANAGER.ast_from_file(path, modname, source=True)
        except astroid.AstroidSyntaxError as e:
            print(f"SYNTAXERROR {getattr(e.error, 'lineno', 0)}:{getattr(e.error, 'offset', 0)} {e.error}")
            continue
        except Exception as e:  # noqa: BLE001
            print(f"BUILDERROR {type(e).__name__}")
            continue
        out = []
        dump(tree, 0, out)
        print("\n".join(out))


if __name__ == "__main__":
    main()
