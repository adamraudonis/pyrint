# prylint

A Rust reimplementation of [pylint](https://github.com/pylint-dev/pylint)'s
error checking that produces **byte-for-byte identical output** to pylint —
**15–84× faster**.

prylint is not "inspired by" pylint. It is a bug-for-bug port: the same
messages, at the same lines and columns, with the same text, in the same
order, with the same exit codes — verified byte-identically against real
pylint on **27 production codebases** (~105,000 messages across ~60,000
Python files), including django, numpy, pandas, sympy, home-assistant,
sqlalchemy, twisted, and pylint's own functional test suite. Where pylint
has bugs, prylint reproduces them. Where pylint crashes, prylint reports the
same crash message.

## Install

```bash
pip install prylint
```

Requirements: a `python3` (≥3.9) on `PATH` (used only to mirror pylint's
module-resolution paths and to reproduce CPython's exact syntax-error
messages for unparseable files). pylint and astroid themselves are **not**
required.

## Usage

prylint targets pylint's errors-only mode. Use it exactly like pylint:

```bash
prylint . -E --disable=C0301,W0511,E0110,E0401,E0611,E1101,...
```

Output, message order, and exit codes match
`pylint . -E --disable=...` (pylint 4.0.5).

## Benchmarks

Measured against pylint 4.0.5 (single run, Apple M-series, 10 cores —
prylint currently uses one core; parallel mode is in development):

| codebase | pylint | prylint | speedup |
|---|---:|---:|---:|
| home-assistant (17.5k files) | 1357s | 16.2s | **84×** |
| airflow | 286s | 7.2s | 40× |
| salt | 183s | 4.3s | 43× |
| sentry | 323s | 8.6s | 38× |
| pandas | 259s | 7.6s | 34× |
| numpy | 194s | 7.2s | 27× |
| scikit-learn | 128s | 5.0s | 26× |
| matplotlib | 65s | 2.9s | 23× |
| django | 109s | 5.4s | 20× |
| sqlalchemy | 84s | 4.2s | 20× |
| zulip | 61s | 3.0s | 20× |
| twisted | 44s | 2.1s | 21× |
| mypy | 42s | 2.1s | 20× |
| nova (OpenStack) | 124s | 6.6s | 19× |
| sympy | 174s | 11.1s | 16× |
| *…and 12 more, all ≥15×* | | | |
| **total (27 repos)** | **3617s** | **103s** | **35×** |

Every row above is also an accuracy test: each repo's full output is
byte-identical to pylint's.

## How accuracy is verified

prylint was built by differential testing against pinned pylint
4.0.5 / astroid 4.0.4 / CPython 3.12:

1. **AST fidelity** — prylint's parse tree (built on the
   [ruff](https://github.com/astral-sh/ruff) parser) is compared
   node-by-node against astroid's (positions, scopes, locals tables, brain
   transforms) across all corpus files: zero differences.
2. **Inference fidelity** — astroid's inference engine (the part that powers
   checks like `not-callable` and `no-value-for-parameter`) is ported
   exactly: lazy generator semantics, the 100-result cap, cache-eviction
   order, `Uninferable` propagation. Verified by dumping the inference
   result of every name/attribute/call node in every corpus file and
   comparing against astroid: byte-identical.
3. **Output fidelity** — full runs compared byte-for-byte, including message
   order, module headers, `# pylint:` pragma handling (disable/enable
   blocks, `disable-next`, `skip-file`), and exit-code bitmasks.
4. **Blind testing** — two batteries of 10 repos each were added *after*
   development and judged cold; every divergence was root-caused and fixed.

### Determinism notes (where pylint disagrees with itself)

Two corners of pylint's output are nondeterministic between its own runs;
prylint pins them:

- **Crash reports** (`F0002`) embed a wall-clock filename
  (`pylint-crash-<timestamp>.txt`). prylint emits the same message shape
  with its own timestamp.
- A few multi-message orderings iterate Python `set`s, so they depend on
  `PYTHONHASHSEED`. prylint reproduces CPython's iteration order for
  `PYTHONHASHSEED=0` exactly (verified by fuzzing against the real
  interpreter), so it matches `PYTHONHASHSEED=0 pylint …` byte-for-byte.

## Scope and limitations

- **Errors-only mode**: prylint implements `pylint -E` (all E/F checks
  except `E0110`, `E0401`, `E0611`, `E1101`) plus the message-control
  machinery (`--disable`, inline pragmas — including pragmas that re-enable
  messages locally). W/R/C checkers are not implemented except where inline
  pragmas in real code resurrect them.
- **Pinned semantics**: behavior matches pylint 4.0.5 / astroid 4.0.4
  running on CPython 3.12. Newer Python syntax (3.13/3.14) is reported as a
  syntax error, exactly as that pylint would.
- **Config files are not read**: `--rcfile` is accepted but ignored;
  pyproject.toml/pylintrc discovery is not implemented yet. Pass options on
  the command line.
- Linted-code dependencies do not need to be installed (matching how the
  benchmark ground truth was produced). If your pylint runs with your
  project's full virtualenv on `sys.path`, set `PRYLINT_PYTHON` to that
  venv's interpreter for identical import resolution.

## How it works

- File discovery, message control, and reporting are direct ports of
  pylint's own logic (down to `os.walk` ordering and the
  `************* Module` header rules).
- Parsing uses ruff's Rust parser, then rebuilds astroid's exact tree shape
  (docstring extraction, decorator positions, implicit class locals,
  metaclass handling, brain transforms for dataclasses/enums/namedtuples…).
- A full port of astroid's inference engine resolves names, calls,
  attributes, MROs, and operator protocols with astroid's exact
  conservatism — including its caches and their quirks, because the quirks
  are observable in the output.
- Files the Rust parser rejects are re-judged by CPython itself (an
  embedded, stdlib-only helper) so syntax-error messages match `ast.parse`
  exactly.

## The target invocation

prylint's accuracy contract is defined against this exact command:

```
pylint . -E --disable=C0301,W0511,C0114,R0402,C0116,R0914,W0718,R1735,W0105,R1705,W0603,W0104,C0209,W0719,C0411,C0412,R0912,R0915,C0413,C0115,C0103,W0613,R0801,W0602,R0913,R0917,W0622,R0902,R0911,R0913,R1702,R1716,W0212,R1728,C0121,R0916,C0415,W1401,C0206,C0302,R0904,W1514,R0903,E0110,R1714,W0707,R1718,W1309,W1203,E0611,W0611,W0108,W0177,E1101,C1803,R1721,W0123,R1720,R1710,W0221,W0122,C0201,W1510,R1729,R1737,C0325,R0401,E0401
```

## Development

The differential-testing harness (corpus ground truth, AST/inference dump
comparators, per-code diff triage) lives in `harness/`. The accuracy
contract: any change must keep all 27 corpora byte-identical.

## License

GPL-2.0-or-later, the same license as [pylint](https://github.com/pylint-dev/pylint)
itself — prylint reproduces pylint's message texts and behavior verbatim.
