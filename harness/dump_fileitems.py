"""Dump the exact (name, filepath) sequence pylint would lint for `pylint .`.

Usage: dump_fileitems.py <dir> — run with the pinned venv python, cwd is set to <dir>.
"""

import json
import os
import sys

os.chdir(sys.argv[1])

from pylint.lint.run import Run

run = Run(
    ["--errors-only", "--rcfile=" + os.devnull, "."],
    exit=False,
    do_exit=False,
) if False else None

# Build linter without running checks
from pylint.lint.pylinter import PyLinter
from pylint.lint.base_options import _make_linter_options
from pylint import config

linter = PyLinter()
linter.load_default_plugins()
# emulate config defaults; no rcfile
from pylint.config.config_initialization import _config_initialization

_config_initialization(linter, ["."], reporter=None, config_file=None, verbose_mode=False)

items = list(linter._iterate_file_descrs(tuple(linter.config.recursive and linter._discover_files(["."]) or ["."])))
for it in items:
    print(json.dumps({"name": it.name, "path": it.filepath}))
