#!/bin/bash
# One-command release:
#   1. (optionally) verifies byte parity on all 27 corpora
#   2. builds the macOS wheel + sdist locally (maturin)
#   3. uploads them to PyPI immediately (twine, uses ~/.pypirc)
#   4. tags vX.Y.Z and pushes main + the tag to GitHub, which triggers
#      .github/workflows/release.yml to build Linux/Windows/macOS-x86_64
#      wheels and publish them via Trusted Publishing (skip-existing, so
#      the locally-uploaded artifacts are never clobbered).
#
# Usage: scripts/release.sh 0.2.0 [--skip-verify]
#
# Publishing from Actions uses the PYPI_API_TOKEN repo secret (already
# configured via gh). Local uploads (step 3) use the token in ~/.pypirc.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

VERSION="${1:?usage: scripts/release.sh X.Y.Z [--skip-verify]}"
SKIP_VERIFY="${2:-}"
# Packaging tools live in a DEDICATED venv. Never pip-install anything into
# .venv-pylint: it is the parity interpreter (PRYLINT_PYTHON) whose
# site-packages define module resolution for the 27-corpus byte-parity gate —
# adding packages there (twine pulls in pygments, requests, rich, ...) changes
# inference results and breaks parity (pip + nova corpora, observed 2026-06-11).
PY=.venv-build/bin/python

# sanity: clean tree, version matches pyproject + workspace
[ -z "$(git status --porcelain)" ] || { echo "working tree not clean"; exit 1; }
grep -q "^version = \"$VERSION\"" pyproject.toml || { echo "pyproject.toml version != $VERSION"; exit 1; }
grep -q "^version = \"$VERSION\"" Cargo.toml || { echo "Cargo.toml workspace version != $VERSION"; exit 1; }

if [ "$SKIP_VERIFY" != "--skip-verify" ]; then
  echo "== verifying byte parity on all 27 corpora (use --skip-verify to skip)"
  cargo build --release
  FAIL=""
  for c in django pandas salt airflow core sentry pylfunc scrapy celery pip \
           fastapi sqlalchemy numpy scikit-learn matplotlib ansible sympy \
           rich tornado werkzeug black botocore mypy pydantic twisted nova zulip; do
    harness/run_prylint.sh $c >/dev/null 2>&1
    if python3 harness/bytecmp.py harness/results/$c.iso.out harness/results/$c.ours.out >/dev/null 2>&1 \
       && [ "$(cat harness/results/$c.iso.exit)" = "$(cat harness/results/$c.ours.exit)" ]; then
      echo "  $c OK"
    else
      echo "  $c BROKEN"; FAIL=1
    fi
  done
  [ -z "$FAIL" ] || { echo "parity broken — refusing to release"; exit 1; }
fi

echo "== building local wheel + sdist"
[ -x "$PY" ] || uv venv --seed -q .venv-build
$PY -m pip install --quiet --upgrade maturin twine
rm -rf dist
$PY -m maturin build --release --out dist
$PY -m maturin sdist --out dist
$PY -m twine check dist/*

echo "== uploading local artifacts to PyPI"
$PY -m twine upload --skip-existing dist/*

echo "== tagging v$VERSION and pushing (triggers multi-platform wheel build + publish)"
git tag -a "v$VERSION" -m "prylint $VERSION"
git push origin HEAD:main
git push origin "v$VERSION"

echo "== done. Watch the wheels build at:"
echo "   https://github.com/adamraudonis/prylint/actions"
