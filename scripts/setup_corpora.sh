#!/bin/bash
# Reproduce prylint's accuracy-test environment: the 27 pinned corpora, the
# pinned pylint/astroid reference sources, and the ground-truth virtualenv.
#
# Usage:
#   scripts/setup_corpora.sh              # venv + references + all corpora
#   scripts/setup_corpora.sh --ground-truth   # ...then regenerate pylint
#                                             # baselines (slow: ~1 hour)
#
# Accuracy contract: pylint 4.0.5 / astroid 4.0.4 / CPython 3.12 /
# PYTHONHASHSEED=0 (exported by harness/ground_truth.sh). Corpora are pinned
# by commit; each is fetched shallowly at its exact SHA.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# ---------------- pinned corpus commits ----------------
CORPORA=(
  "django https://github.com/django/django 17a56cd6e600cfb02972657e8958b5ee2e0f936e"
  "pandas https://github.com/pandas-dev/pandas c79d638e27a10fd2359491bbf7ac1effe8a8b3ff"
  "salt https://github.com/saltstack/salt 57210b6fbd16706bb2bab5f5346050dc13f8c1c7"
  "airflow https://github.com/apache/airflow b311e6fb453a644d9d9c0e55f248786815f999d1"
  "core https://github.com/home-assistant/core 426213dd298221acde7da7fe2f98656bdd8b887c"
  "sentry https://github.com/getsentry/sentry 6f68c1bd403fb6961f4c7cb78d2092d15e0c5ed9"
  "scrapy https://github.com/scrapy/scrapy a8ffdcf8517a8973391a14635234b6993b15a86a"
  "celery https://github.com/celery/celery 4f1595434b7e705e8b9de668dff40b8456621979"
  "pip https://github.com/pypa/pip 486db076e2f4f0bf6780c24cd487f09dc2a14015"
  "fastapi https://github.com/fastapi/fastapi d3e6a2931f95dcb044c09cbfc39e4b0e9e620fd4"
  "sqlalchemy https://github.com/sqlalchemy/sqlalchemy 4e97e0abaf5c980ff04558323974c72757baa46f"
  "numpy https://github.com/numpy/numpy 409000d65bfba5f08e7d23362564d4c68bf1114f"
  "scikit-learn https://github.com/scikit-learn/scikit-learn 3af306497f4070161514860bfd344df165997a83"
  "matplotlib https://github.com/matplotlib/matplotlib 0677cc8c142eae9ea5dde9cd5487c106dff58cbb"
  "ansible https://github.com/ansible/ansible d772fe65b73e3032f88a9915111b58e743af9d9f"
  "sympy https://github.com/sympy/sympy 6fcc30bbfa6fa4cbde2124640f1f84385292c6a6"
  "rich https://github.com/Textualize/rich 46cebbb032f920eb096efbaf23cdc6fe9dd541f7"
  "tornado https://github.com/tornadoweb/tornado f491e4c1914be0ac6635a0eacb3c978d89eec4f1"
  "werkzeug https://github.com/pallets/werkzeug 1b00618e787f40dfb21eba29caf8f8be7c8e1d93"
  "black https://github.com/psf/black 6325332f05312ebde112a61cf4a19ef2ecf1ea74"
  "botocore https://github.com/boto/botocore 72abfc37453049f60113c88e3e2ad398039f217e"
  "mypy https://github.com/python/mypy e0c375a97105ecf43c9ff9b858e01cd6e938a077"
  "pydantic https://github.com/pydantic/pydantic 2700a3594d61844eb771abf2b3e36660d501e0dd"
  "twisted https://github.com/twisted/twisted 7affbcb45246f7713e28a1499dd520d69f83aaf9"
  "nova https://github.com/openstack/nova d8c997c350d745a118b34dfce62701f0013d7a16"
  "zulip https://github.com/zulip/zulip d6481c5ae28318cc7813ab5eef014e4eb46882e2"
)

# ---------------- ground-truth virtualenv ----------------
if [ ! -x .venv-pylint/bin/pylint ]; then
  echo "== creating .venv-pylint (pylint 4.0.5 / astroid 4.0.4, python 3.12)"
  if command -v uv >/dev/null; then
    uv venv --python 3.12 .venv-pylint
    uv pip install --python .venv-pylint/bin/python "pylint==4.0.5" "astroid==4.0.4"
  else
    python3.12 -m venv .venv-pylint
    .venv-pylint/bin/pip install --quiet "pylint==4.0.5" "astroid==4.0.4"
  fi
fi
.venv-pylint/bin/pylint --version

# ---------------- pinned reference sources ----------------
mkdir -p reference
[ -d reference/pylint ] || git clone --depth 1 --branch v4.0.5 https://github.com/pylint-dev/pylint reference/pylint
[ -d reference/astroid ] || git clone --depth 1 --branch v4.0.4 https://github.com/pylint-dev/astroid reference/astroid

# ---------------- corpora at pinned SHAs ----------------
mkdir -p corpora
fetch_pinned() {
  local name="$1" url="$2" sha="$3"
  local dir="corpora/$name"
  if [ -d "$dir/.git" ] && [ "$(git -C "$dir" rev-parse HEAD)" = "$sha" ]; then
    echo "== $name already at $sha"
    return
  fi
  echo "== fetching $name @ $sha"
  rm -rf "$dir"
  mkdir -p "$dir"
  git -C "$dir" init -q
  git -C "$dir" remote add origin "$url"
  # GitHub permits fetching reachable SHAs directly
  git -C "$dir" fetch -q --depth 1 origin "$sha"
  git -C "$dir" checkout -q FETCH_HEAD
}
for entry in "${CORPORA[@]}"; do
  fetch_pinned $entry
done

# pylfunc = pylint's own functional-test corpus, taken from the pinned tag
if [ ! -d corpora/pylfunc ]; then
  echo "== creating pylfunc from reference/pylint tests/functional"
  cp -R reference/pylint/tests/functional corpora/pylfunc
fi

echo "== all corpora ready"

# ---------------- optional: regenerate ground truth ----------------
if [ "${1:-}" = "--ground-truth" ]; then
  echo "== regenerating pylint baselines (this takes ~1 hour, single-threaded)"
  for entry in "${CORPORA[@]}"; do
    set -- $entry
    harness/ground_truth.sh "$1" iso
  done
  harness/ground_truth.sh pylfunc iso
  echo "== ground truth regenerated under harness/results/"
fi

cat <<'EOF'

Next steps:
  cargo build --release
  harness/run_prylint.sh django        # run prylint on a corpus
  python3 harness/bytecmp.py harness/results/django.iso.out harness/results/django.ours.out
  # (regenerate baselines first with --ground-truth if you skipped it)
EOF
