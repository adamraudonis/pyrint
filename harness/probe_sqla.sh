#!/bin/bash
# Isolated pylint probe of the sqlalchemy R0901 divergent classes.
set -u
ROOT=~/Desktop/Projects/prylint
cd "$ROOT/corpora/sqlalchemy" || exit 1
export PYTHONHASHSEED=0
PYL="$ROOT/.venv-pylint/bin/pylint"
echo "=== ISOLATED: just orm/base.py ==="
"$PYL" --rcfile="$ROOT/harness/empty.rcfile" --disable=all --enable=R0901,R0903 \
  lib/sqlalchemy/orm/base.py 2>/dev/null | grep -E "R0901|R0903" || echo "(none)"
echo "=== ISOLATED: just postgresql/array.py ==="
"$PYL" --rcfile="$ROOT/harness/empty.rcfile" --disable=all --enable=R0901,R0903 \
  lib/sqlalchemy/dialects/postgresql/array.py 2>/dev/null | grep -E "R0901|R0903" || echo "(none)"
