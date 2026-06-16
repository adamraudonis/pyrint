# Limitations & known divergences

prylint is byte-for-byte identical to pylint 4.0.5 (astroid 4.0.4, CPython
3.12) on the 27-corpus test suite, with the small, deliberate, or
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

prylint is single-threaded in the inference/checking phase **by default**: the
byte-identical guarantee depends on replicating astroid's process-global,
order-sensitive inference cache (including the bounded-LRU eviction in §2)
exactly. Parallelizing across files changes cache-warming order and
reintroduces divergences — so the default single-core speed (still 15–2300×
pylint) is the *correct, byte-identical* speed. File parsing is already
parallelized within that serial path.

### 5.1 `-j N` (opt-in parallel mode) — trades byte-identity for cores

`-j N` (alias `--jobs N`) runs phase 2 (per-module checks) across `N` worker
threads. **It is opt-in and off by default.** Like pylint's own `-j`, it
**does not** guarantee byte-identical output to the serial run, and prylint
makes no attempt to: the user is explicitly opting into "different but faster."

- **The serial path is untouched.** `-j 1`, or no `-j` at all, runs the *exact*
  existing single-engine code path and stays byte-identical to pylint. The
  parallel branch is only taken for `-j N` with `N>1` (or `-j 0` = auto =
  `available_parallelism()`). The `-E` 27-corpus byte-identity gate still runs
  on the serial path and stays green.

- **Why output differs from serial.** Each worker owns its **own thread-local
  (`Rc`-based) inference cache** — there is no shared concurrent cache. Every
  worker boots *all* files in file order (mirroring pylint's phase-1 cache
  warming) but *walks* only its fixed round-robin residue class
  (`file_index % N == k`). Consequences:
  - **Cache warmth differs.** A worker only re-checks 1/N of the files, so the
    interleaving of astroid-style transform-cache wipes and inference warming
    at each check moment differs from the single serial run. This is the same
    class of warm-cache order effect catalogued in §2/§3 — surfaced
    deliberately here instead of being suppressed.
  - **Cross-shard / cross-module checks see only one shard.** The whole-run
    "close" checks — `R0801` (duplicate-code) and `R0401` (cyclic-import) —
    run on worker 0 only, over *that worker's* file subset, so they report far
    fewer findings than serial. On django (full mode, 10-core M-series),
    serial vs `-j 8` differs by ~938 message lines, of which **592 are R0801
    and 344 are R0401** — i.e. the divergence is overwhelmingly these two
    cross-file checks; only 2 lines (`C0114`) come from anything else.

- **Deterministic per run (Invariant #2).** The file partition is a *fixed*
  `file_index % N` residue class, not a work-stealing queue, and per-worker
  outputs are merged and flushed strictly in original file order — so module
  headers, the score, and the exit bitmask are computed over the merged result
  exactly as serial does. The same input therefore produces the same `-j N`
  output every run (verified byte-identical across repeated `-j 8` runs on
  django with `--persistent=n`). It is *not* identical to `-j 1`; it *is*
  identical to itself. (The `Your code has been rated … (previous run: …)`
  footer still reflects the persisted prior-run score, exactly as in pylint —
  that is not run-to-run nondeterminism in the findings.)

- **Speedup is modest and partition-bound.** Because determinism requires each
  worker to boot the *full* file set (the per-worker engine-boot is recomputed,
  ~2s/worker on django), wall-clock speedup is capped. On django (full mode,
  adam disable-profile, 10-core M-series): serial ~15.4s → `-j 8` ~13.7s
  (~1.1×); `-j 0` (auto) ~14.5s. The win grows on inference-heavy corpora where
  the per-module check dominates the boot cost; `-j N` is most useful when the
  serial single-core time is already long and exact parity is not required.
  When you need byte-identity to pylint, use the default (serial) path.

In short: **default = serial = byte-identical to pylint; `-j N` = opt-in,
deterministic per run, but may differ from serial.**

## Scope of "byte-identical"

On the 27-corpus suite (both the errors-only and a full-disable profile):
`-E` mode is byte-identical on all 27. Full mode is byte-identical on 26/27 in
the hook profile and 27/27 in the errors-only profile, with the §2 exception
(one class) and §1/§3 documented divergences. A small number of false
negatives may exist beyond §1 on code that stresses inference in ways not
covered by the suite.
