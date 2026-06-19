# Limitations & known divergences

prylint is byte-for-byte identical to pylint 4.0.5 (astroid 4.0.4, CPython
3.12) on the 52-corpus test suite, with the small, deliberate, or
fundamentally-irreducible exceptions catalogued here. `-E` (errors-only) mode
is **unconditionally** byte-identical; the exceptions below are full-check-mode
only unless noted.

## 1. `no-member` family is deliberately excluded

`E1101` (no-member) and `I1101` (c-extension-no-member) are **not implemented**,
by design — they require resolving every attribute access against full
cross-module/C-extension type inference, which is the single most expensive
check in pylint for very little real-world signal (it is one of the most
commonly disabled pylint messages). prylint therefore does not emit them, which
shows up as false negatives where pylint would (e.g. pygments filter internals
in pip's vendored tree). Related `E0611` (no-name-in-module) on packages that
build their namespace via dynamic re-export (`from . import x`) can also be
missed.

## 2. Two known false positives (one obscure SQLAlchemy class)

In **full mode**, prylint emits 2 false positives that pylint does not, both on
`sqlalchemy.dialects.postgresql.array` (`array.py:93`), in the hook/disable
profile:

- `R0901` too-many-ancestors (counts 43 ancestors vs pylint's 7)
- `W0223` abstract-method (`__sa_operate__`)

Root cause (fully traced): both flow from one inference-cache entry —
`OperatorExpression[_T]` at `sql/elements.py:3113:25` — that astroid keeps
`Uninferable` but prylint resolves. The cold inference is byte-identical to
astroid; the divergence is a **cross-file cache cap-cliff**: the result depends
on the exact interleaving of astroid's transform-cache wipes and inference
warming across thousands of files, and the value provably oscillates depending
on cache state at the moment of the check. Matching it byte-for-byte would
require reproducing astroid's whole-run wipe/warm sequence, which risks the
unconditional `-E` byte-identity guarantee for a 2-message cosmetic gain on one
dependency-internal class. Left as a documented exception.

## 3. Places pylint is nondeterministic against itself

These are not prylint bugs — **two runs of real pylint disagree here**, so
byte-identity to any single run is impossible by construction. prylint pins
each to a stable, defensible choice.

- **`R0801` duplicate-code block content.** pylint picks the printed
  representative file-pair and source block via set iteration over `LineSet`
  objects keyed by `id(self)` (heap address). Two `PYTHONHASHSEED=0` pylint
  runs on scrapy differ by 345 lines, on salt by 9,393 lines, with the *same*
  R0801 message count. The set of duplicate-code findings is deterministic; the
  printed block is not.
- **`F0002` crash reports** embed a wall-clock crash-file path
  (`pylint-crash-<timestamp>.txt` under `PYLINTHOME`). prylint emits the same
  message shape with its own path/timestamp. (prylint reproduces pylint's
  crashes themselves — e.g. the `UnicodeDecodeError` in the logging checker on
  pip, and the deep-recursion crash on sympy's Galois resolvents.)
- **Set-ordered multi-message output** (e.g. duplicate-keyword reporting)
  depends on `PYTHONHASHSEED`. prylint reproduces CPython's iteration order for
  `PYTHONHASHSEED=0` exactly (verified by fuzzing against the interpreter), so
  it matches `PYTHONHASHSEED=0 pylint …`.

## 4. Pinned semantics

Behavior matches **pylint 4.0.5 / astroid 4.0.4 on CPython 3.12**. Newer Python
syntax (3.13/3.14 — t-strings, PEP 696 defaults, …) is reported as a syntax
error, exactly as that pylint would. Plugins loaded via `load-plugins` and
their messages are not implemented.

## 5. Concurrency

prylint runs the inference/checking phase single-threaded: the byte-identical
guarantee depends on replicating astroid's process-global, order-sensitive
inference cache (including the bounded-LRU eviction in §2) exactly.
Parallelizing across files changes cache-warming order and reintroduces
divergences — so the single-core speed (still 15–2300× pylint) is the
*correct, byte-identical* speed. File parsing is already parallelized within
that serial path.

For compatibility with existing pylint command lines and pre-commit hooks,
`-j N` / `--jobs N` (including `-j 0`) is **accepted but ignored**: prylint
always runs the serial, byte-identical path.

## Scope of "byte-identical"

On the 52-corpus suite (both the errors-only and a full-disable profile):
`-E` (errors-only) mode is byte-identical on all 52. Full check mode is
byte-identical on every corpus except the single SQLAlchemy class in §2, with
the §1/§3 documented divergences. A small number of false negatives may exist
beyond §1 on code that stresses inference in ways not covered by the suite.
