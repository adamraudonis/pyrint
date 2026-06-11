#!/bin/bash
# Standalone-binary smoke test: copies target/release/prylint ALONE into an
# empty temp dir (no repo checkout, no PRYLINT_SNAPSHOT_DIR/PRYLINT_ORACLE),
# creates a small project exercising the embedded oracle (syntax-error file,
# t-string file, import of a broken module) and the embedded snapshots
# (math.pi -> E1102 needs the math C-ext snapshot), then byte-compares
# against .venv-pylint pylint run with the exact harness/flags.txt flags.
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${1:-$ROOT/target/release/prylint}"
SMOKE=$(mktemp -d "${TMPDIR:-/tmp}/prylint-standalone-XXXXXX")
trap 'rm -rf "$SMOKE"' EXIT

cp "$BIN" "$SMOKE/prylint-bin"
mkdir "$SMOKE/proj"
cd "$SMOKE/proj" || exit 1

cat > broken.py <<'EOF'
def f(:
    pass
EOF
cat > tstr.py <<'EOF'
name = "x"
t = t"hello {name}"
EOF
cat > main.py <<'EOF'
import math
from broken import thing
from collections import OrderedDict

math.pi()
d = OrderedDict()
print(undefined_name)
EOF

FLAGS=$(cat "$ROOT/harness/flags.txt")
# Both sides on the pinned interpreter: verdicts (e.g. the t-string file)
# are interpreter-version-dependent by design, for prylint exactly as for
# pylint itself.
PYTHONHASHSEED=0 "$ROOT/.venv-pylint/bin/pylint" . $FLAGS > ../iso.out 2> ../iso.err
echo $? > ../iso.exit
env -u PRYLINT_SNAPSHOT_DIR -u PRYLINT_ORACLE \
    PRYLINT_PYTHON="$ROOT/.venv-pylint/bin/python" \
    ../prylint-bin . $FLAGS > ../ours.out 2> ../ours.err
echo $? > ../ours.exit
cd ..

FAIL=0
python3 "$ROOT/harness/bytecmp.py" iso.out ours.out > /dev/null || FAIL=1
[ "$(cat iso.exit)" = "$(cat ours.exit)" ] || FAIL=1
[ -s ours.err ] && { echo "unexpected prylint stderr:"; cat ours.err; FAIL=1; }
# the project must actually produce messages (guard against a silent no-op)
grep -q "E1102" iso.out || { echo "smoke project produced no E1102?"; FAIL=1; }

if [ "$FAIL" = 0 ]; then
    echo "standalone smoke: PASS (exit $(cat iso.exit), $(wc -l < iso.out | tr -d ' ') lines byte-identical)"
else
    echo "standalone smoke: FAIL"
    echo "--- iso.out ---"; cat iso.out
    echo "--- ours.out ---"; cat ours.out
    echo "exits: iso=$(cat iso.exit) ours=$(cat ours.exit)"
    exit 1
fi
