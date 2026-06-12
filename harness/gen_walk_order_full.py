import os, sys, io, json
flags = ["--rcfile=" + os.path.expanduser("~/Desktop/Projects/prylint/harness/empty.rcfile")]
from pylint.lint.run import Run
sys.stdout, real = io.StringIO(), sys.stdout
run = Run(flags + [os.devnull], exit=False)
sys.stdout = real
linter = run.linter
from pylint.utils.ast_walker import ASTWalker
from pylint.checkers import BaseTokenChecker, BaseRawFileChecker
w = ASTWalker(linter)
cks = linter.prepare_checkers()
out = []
out.append("// FULL-mode (no -E, empty rcfile) walk order")
for c in cks:
    w.add_checker(c)
out.append("pub static VISIT_ORDER_FULL: &[(&str, &[&str])] = &[")
for cid in sorted(w.visit_events):
    cbs = ", ".join(f'"{cb.__self__.__class__.__name__}.{cb.__func__.__name__}"' for cb in w.visit_events[cid])
    out.append(f'    ("{cid}", &[{cbs}]),')
out.append("];")
out.append("")
out.append("pub static LEAVE_ORDER_FULL: &[(&str, &[&str])] = &[")
for cid in sorted(w.leave_events):
    cbs = ", ".join(f'"{cb.__self__.__class__.__name__}.{cb.__func__.__name__}"' for cb in w.leave_events[cid])
    out.append(f'    ("{cid}", &[{cbs}]),')
out.append("];")
out.append("")
raws = [c for c in cks if isinstance(c, BaseRawFileChecker)]
toks = [c for c in cks if isinstance(c, BaseTokenChecker)]
out.append(f'pub static RAW_CHECKERS_FULL: &[&str] = &[{", ".join(chr(34)+type(c).__name__+chr(34) for c in raws)}];')
out.append(f'pub static TOKEN_CHECKERS_FULL: &[&str] = &[{", ".join(chr(34)+type(c).__name__+chr(34) for c in toks)}];')
print("\n".join(out))
