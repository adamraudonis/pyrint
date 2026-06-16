# prylint

A Rust reimplementation of [pylint](https://github.com/pylint-dev/pylint) that
produces **byte-for-byte identical output** — **15–2300× faster** (median ~85×).

prylint is not "inspired by" pylint. It is a bug-for-bug port: the same
messages, at the same lines and columns, with the same text, in the same
order, with the same exit codes and the same `Your code has been rated`
footer — verified byte-identically against real pylint on **27 production
codebases** (~60,000 Python files), including django, numpy, pandas, sympy,
home-assistant, sqlalchemy, twisted, scikit-learn, and pylint's own functional
test suite. Where pylint has bugs, prylint reproduces them. Where pylint
crashes, prylint reports the same crash message.

## Install

```bash
pip install prylint
```

Requirements: a `python3` (≥3.9) on `PATH` (used only to mirror pylint's
module-resolution paths and to reproduce CPython's exact syntax-error messages
for unparseable files). pylint and astroid themselves are **not** required.

## Usage

Use it exactly like pylint — full check mode is the default:

```bash
prylint .                      # all checks (like `pylint .`)
prylint -E .                   # errors only (like `pylint -E .`)
prylint --disable=C0114,... .  # same --disable / --enable / inline pragmas
```

Output, message order, exit codes, the score footer, `--rcfile` /
`pyproject.toml` discovery, `init-hook`, and `# pylint:` pragmas all match
pylint 4.0.5.

## Benchmarks

`prylint .` vs `pylint .` (both full check mode), pylint 4.0.5, Apple M-series,
single-threaded:

| codebase | pylint | prylint | speedup |
|---|---:|---:|---:|
| black | 26.7 hr | 41s | **2328×** |
| sentry | 3.7 hr | 24s | 546× |
| home-assistant (17.5k files) | 10.3 hr | 82s | 452× |
| airflow | 1.9 hr | 17s | 399× |
| salt | 1890s | 8.8s | 215× |
| zulip | 909s | 5.3s | 172× |
| django | 1524s | 10.1s | 150× |
| ansible | 419s | 2.9s | 143× |
| nova (OpenStack) | 1209s | 10.3s | 117× |
| fastapi | 116s | 1.0s | 120× |
| mypy | 367s | 3.9s | 95× |
| sqlalchemy | 614s | 7.1s | 87× |
| pandas | 1009s | 14.2s | 71× |
| scikit-learn | 613s | 9.6s | 64× |
| sympy | 1238s | 26s | 48× |
| *…and 12 more, all ≥30×* | | | |
| **aggregate (27 repos)** | **45.8 hr** | **4.9 min** | **~560×** |

Median per-repo speedup ~85×; the aggregate is higher because pylint's
duplicate-code check (`R0801`) is O(n²) and dominates on test-heavy repos like
black. **This is single-core** — the inference engine is deliberately
single-threaded to replicate astroid's order-sensitive global cache exactly
(see [LIMITATIONS.md](LIMITATIONS.md)).

Every row above is also an accuracy test: each repo's full output is
byte-identical to pylint's (see exceptions in [LIMITATIONS.md](LIMITATIONS.md)).

## Accuracy

prylint was built by differential testing against pinned pylint 4.0.5 /
astroid 4.0.4 / CPython 3.12:

1. **AST fidelity** — prylint's parse tree (built on the
   [ruff](https://github.com/astral-sh/ruff) parser) is compared node-by-node
   against astroid's (positions, scopes, locals, brain transforms) across all
   corpus files: zero differences.
2. **Inference fidelity** — astroid's inference engine is ported exactly:
   lazy-generator semantics, the 100-node inference budget, the bounded-LRU
   caches (`lookup` 128, `_metaclass_lookup_attribute` 1024) with their exact
   eviction, the 64-entry inference-tip FIFO, `Uninferable` propagation. Every
   name/attribute/call node's inference is dumped and compared against astroid.
3. **Output fidelity** — full runs compared byte-for-byte, including message
   order, module headers, the score footer, `# pylint:` pragma handling
   (disable/enable blocks, `disable-next`, `skip-file`), config-file discovery,
   and exit-code bitmasks.
4. **Blind testing** — two batteries of 10 repos each were added *after*
   development and judged cold; every divergence was root-caused and fixed.

Known, documented exceptions (one obscure SQLAlchemy class; the deliberately
excluded `no-member` family; the places pylint is nondeterministic against
itself) are catalogued in **[LIMITATIONS.md](LIMITATIONS.md)**.

## How it works

- File discovery, message control, config parsing, and reporting are direct
  ports of pylint's own logic (down to `os.walk` ordering, the
  `************* Module` header rule, and the score-report footer).
- Parsing uses ruff's Rust parser, then rebuilds astroid's exact tree shape
  (docstring extraction, decorator positions, implicit class locals, metaclass
  handling, brain transforms for dataclasses/enums/namedtuples/attrs/…).
- A full port of astroid's inference engine resolves names, calls, attributes,
  MROs, and operator protocols with astroid's exact conservatism — including
  its caches and their quirks, because the quirks are observable in the output.
- Files the Rust parser rejects are re-judged by CPython itself (an embedded,
  stdlib-only helper) so syntax-error messages match `ast.parse` exactly.

## Reproducing the test suite

`scripts/setup_corpora.sh` clones all 27 corpora at pinned commits and builds
the pinned pylint/astroid ground-truth venv. The accuracy contract: every
change must keep the corpora byte-identical (`harness/` holds the differential
comparators).

## License

GPL-2.0-or-later, the same license as
[pylint](https://github.com/pylint-dev/pylint) — prylint reproduces pylint's
message texts and behavior verbatim.
