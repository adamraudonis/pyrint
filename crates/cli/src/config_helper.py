"""Stdlib-only config-file discovery + parsing helper (Phase F).

Mirrors pylint 4.0.5's config layer (notes/09-pipeline-noE.md §8) using the
SAME stdlib machinery pylint uses (configparser, tomllib), so INI/TOML edge
semantics (interpolation, DEFAULT-section merge, comment prefixes, the
setup.cfg/tox.ini section filter) are bug-for-bug identical without porting a
parser to Rust. prylint shells out once at startup.

Operations (one JSON request on stdin, one JSON response on stdout):

  {"op":"discover","cwd":"..."}            -> {"path": <first default config or null>}
       find_default_config_files() FIRST yield (find_default_config_files.py),
       cwd-relative CONFIG_NAMES order + content checks.

  {"op":"parse","path":"..."}              -> {"options": {<name>:[<values>...]}, "init_hooks":[...], "err": null|str}
       parse_config_file (config_file_parser.py): .toml -> TOML parser,
       everything else -> INI parser. Returns each option name mapped to the
       LIST of its occurrences (disable/enable accumulate). 'init-hook' is
       returned separately (executed before the rest). 'err' set on a
       configparser/TOML decode error (the F0011 path) — caller emits F0011.
"""

import configparser
import json
import os
import sys

try:
    import tomllib
except ModuleNotFoundError:  # py<3.11 (not the pinned runtime)
    tomllib = None


CONFIG_NAMES = [
    "pylintrc",
    "pylintrc.toml",
    ".pylintrc",
    ".pylintrc.toml",
    "pyproject.toml",
    "setup.cfg",
    "tox.ini",
]
RC_NAMES = ["pylintrc", "pylintrc.toml", ".pylintrc", ".pylintrc.toml"]


def _toml_has_config(path):
    if tomllib is None:
        return False
    try:
        with open(path, "rb") as f:
            content = tomllib.load(f)
    except tomllib.TOMLDecodeError as e:
        print(f"Failed to load '{path}': {e}")
        return False
    return "pylint" in content.get("tool", [])


def _cfg_or_ini_has_config(path):
    parser = configparser.ConfigParser()
    try:
        parser.read(path, encoding="utf-8")
    except configparser.Error:
        return False
    return any(
        section.startswith("pylint.") or section == "pylint"
        for section in parser.sections()
    )


def _yield_default_files(cwd):
    for name in CONFIG_NAMES:
        path = os.path.join(cwd, name)
        try:
            if not os.path.isfile(path):
                continue
        except OSError:
            continue
        if name.endswith(".toml"):
            if _toml_has_config(path):
                yield os.path.realpath(path)
        elif name.endswith((".cfg", ".ini")) or name in ("setup.cfg", "tox.ini"):
            if _cfg_or_ini_has_config(path):
                yield os.path.realpath(path)
        else:
            yield os.path.realpath(path)


def op_discover(cwd):
    # Only the FIRST default config file is used (run.py:167-170 next(..., None)).
    # We implement the cwd-relative _yield_default_files stage (the dominant
    # case for real projects); the ancestor/home/etc fallbacks are rarely the
    # winning candidate when a project has its own config.
    for path in _yield_default_files(cwd):
        return {"path": path}
    return {"path": None}


def _parse_ini(path):
    parser = configparser.ConfigParser(inline_comment_prefixes=("#", ";"))
    with open(path, encoding="utf_8_sig") as f:
        parser.read_file(f, source=path)
    options = {}
    parts = path.replace("\\", "/").split("/")
    restrict = "setup.cfg" in parts or "tox.ini" in parts
    for section in parser.sections():
        if restrict and not (section == "pylint" or section.startswith("pylint.")):
            continue
        for opt, value in parser.items(section):
            options.setdefault(opt, []).append(value)
    return options


def _flatten(value):
    if isinstance(value, (list, tuple)):
        return ",".join(_flatten(v) for v in value)
    if isinstance(value, dict):
        return ",".join(f"{k}:{v}" for k, v in value.items())
    if isinstance(value, bool):
        return "True" if value else "False"
    return str(value)


def _parse_toml(path):
    if tomllib is None:
        return {}
    with open(path, "rb") as f:
        content = tomllib.load(f)
    pylint = content.get("tool", {}).get("pylint", {})
    options = {}
    for key, value in pylint.items():
        if isinstance(value, dict):  # [tool.pylint.section]
            for k, v in value.items():
                options.setdefault(k, []).append(_flatten(v))
        else:
            options.setdefault(key, []).append(_flatten(value))
    return options


def op_parse(path):
    try:
        path = os.path.expandvars(os.path.expanduser(path))
        if path.endswith(".toml"):
            options = _parse_toml(path)
        else:
            options = _parse_ini(path)
    except (configparser.Error, FileNotFoundError, OSError) as e:
        # FileNotFoundError on --rcfile -> exit 32 (handled by caller via err);
        # configparser.Error -> F0011 (caller).
        return {"options": {}, "init_hooks": [], "err": str(e), "kind": type(e).__name__}
    except Exception as e:
        if tomllib is not None and isinstance(e, tomllib.TOMLDecodeError):
            return {"options": {}, "init_hooks": [], "err": str(e), "kind": "TOMLDecodeError"}
        return {"options": {}, "init_hooks": [], "err": str(e), "kind": type(e).__name__}
    # init-hook is exec'd after utils._unquote (config_initialization.py:54):
    # strip ONE optional leading and trailing quote (' or ").
    init_hooks = [_unquote(h) for h in options.pop("init-hook", [])]
    return {"options": options, "init_hooks": init_hooks, "err": None}


def _unquote(s):
    """pylint utils._unquote: drop one optional leading and trailing quote."""
    if not s:
        return s
    if s[0] in "\"'":
        s = s[1:]
    if s and s[-1] in "\"'":
        s = s[:-1]
    return s


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
            op = req.get("op")
            if op == "discover":
                resp = op_discover(req["cwd"])
            elif op == "parse":
                resp = op_parse(req["path"])
            else:
                resp = {"err": "unknown op"}
        except Exception as ex:
            resp = {"err": str(ex)}
        sys.stdout.write(json.dumps(resp) + "\n")
        sys.stdout.flush()


if __name__ == "__main__":
    main()
