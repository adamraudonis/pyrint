#!/usr/bin/env python3
"""dump_infer.py variant printing the per-node nodes_inferred counter
(`##N` suffix) — used to localize counter-dynamics divergences between
astroid and pyinfer. Usage: dump_infer_count.py <root> <items.jsonl> [--only ...]"""
import sys, os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import dump_infer
from astroid.context import InferenceContext
import astroid

_orig_infer_node = dump_infer.infer_node

def infer_node_count(n):
    ctx = InferenceContext()
    try:
        vals = [dump_infer.render(v) for v in n.infer(context=ctx)]
    except astroid.InferenceError:
        vals = ["ERR"]
    except RecursionError:
        vals = ["RECURSION"]
    except Exception as e:  # noqa: BLE001
        vals = [f"CRASH:{type(e).__name__}"]
    vals[-1] = vals[-1] + f" ##{ctx.nodes_inferred}" if vals else f" ##{ctx.nodes_inferred}"
    return vals

dump_infer.infer_node = infer_node_count

if __name__ == "__main__":
    dump_infer.main()
