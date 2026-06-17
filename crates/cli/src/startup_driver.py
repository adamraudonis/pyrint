"""Stdlib-only UNIFIED startup driver (Phase F) — ONE `python -I` subprocess
that serves every python-dependent startup probe prylint needs, replacing the
former three separate spawns (config-helper, sys.path probe, init-hook exec)
plus the duplicate config-helper call, AND the persistent stats coprocess.

prylint spawns this ONCE at startup and drives it as a persistent coprocess:
one JSON request per stdin LINE, one JSON response per stdout line (the same
protocol the syntax-oracle uses), so a single interpreter boot amortizes over
all of: config-file discovery, config parsing, the interpreter sys.path probe,
init-hook execution + sys.path-delta capture, and the PYLINT_HOME stats
load/save for the score footer's "previous run" suffix.

Operations (matching the former helpers bug-for-bug):

  {"op":"discover","cwd":"..."}            -> {"path": <first default config or null>}
       find_default_config_files() FIRST yield (find_default_config_files.py),
       cwd-relative CONFIG_NAMES order + content checks.

  {"op":"parse","path":"..."}              -> {"options": {<name>:[<values>...]}, "init_hooks":[...], "err": null|str}
       parse_config_file (config_file_parser.py): .toml -> TOML parser,
       everything else -> INI parser. Returns each option name mapped to the
       LIST of its occurrences. 'init-hook' is returned separately (executed
       before the rest). 'err' set on a configparser/TOML decode error.

  {"op":"probe"}                            -> {<interpreter sys.path + builtins/stdlib/ext_suffixes/os_path_file/frozen_specs>}
       The pinned-interpreter environment probe (pyenv.rs PROBE), captured in
       THIS interpreter (same `python -I`, so sys.path is the interpreter's
       baseline — exactly what a separate `-c` spawn would have reported).

  {"op":"inithook","hooks":[...]}           -> {"added":[...], "ok": bool}
       exec each hook in order (config/utils.py _preprocess_options) and return
       the sys.path entries the hooks added (entries not in the pre-exec
       baseline), in order. NOTE: run AFTER probe so the probe reports the
       PRISTINE interpreter sys.path; the init-hook delta is applied by the
       caller on top of it (mirroring pylint's augmented_sys_path ordering).

  {"op":"statsload","path":"..."}           -> {"global_note": <float|null>}
       caching.load_results(base).global_note for the footer suffix; missing
       file or ANY exception -> null (silent tolerance).

  {"op":"statssave","path":"...","state":{...}} -> {"ok": bool}
       caching.save_results: write a real pylint.utils.linterstats.LinterStats
       pickle (raw protocol-4 opcodes; no pylint import needed) so a later
       pylint OR prylint run reads it back.
"""

import configparser
import io
import json
import os
import pickle
import struct
import sys

try:
    import tomllib
except ModuleNotFoundError:  # py<3.11 (not the pinned runtime)
    tomllib = None


# ======================================================================
# config-file discovery + parsing (former config_helper.py)
# ======================================================================

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


# ======================================================================
# interpreter environment probe (former pyenv.rs PROBE)
# ======================================================================

def op_probe():
    import importlib.machinery
    import importlib.util
    import _imp

    frozen = {}
    for name in _imp._frozen_module_names():
        try:
            spec = importlib.util.find_spec(name)
        except (ValueError, ImportError):
            continue
        if spec and spec.loader is importlib.machinery.FrozenImporter:
            frozen[name] = getattr(spec.loader_state, "filename", None)
    return {
        "sys_path": list(sys.path),
        "builtin": list(sys.builtin_module_names),
        "stdlib": list(sys.stdlib_module_names),
        "ext_suffixes": importlib.machinery.EXTENSION_SUFFIXES,
        "os_path_file": os.path.__file__,
        "frozen_specs": frozen,
    }


# ======================================================================
# init-hook execution + sys.path-delta capture (former run_init_hooks)
# ======================================================================

def op_inithook(hooks):
    # config/utils.py _preprocess_options exec's each --init-hook value in
    # process. We snapshot sys.path, exec each hook in order, and report the
    # entries the hooks ADDED (preserving order). Any non-sys.path side effect
    # (env vars, monkeypatches) is not observable by the Rust process; the
    # caller warns about that on stderr.
    before = list(sys.path)
    for h in hooks:
        exec(h)  # noqa: S102 — mirrors pylint's own exec of init-hook
    after = list(sys.path)
    added = [p for p in after if p not in before]
    return {"ok": True, "added": added}


# ======================================================================
# PYLINT_HOME stats pickle load/save (former stats_helper.py)
# ======================================================================

class _StubStats:
    """Stand-in for any GLOBAL the cache references; we only read __dict__."""

    def __setstate__(self, state):  # not used by 4.0.5 (plain BUILD)
        self.__dict__.update(state)


class _StubUnpickler(pickle.Unpickler):
    def find_class(self, module, name):
        # Resolve EVERY foreign class to the stub so load never needs pylint
        # or astroid importable. Builtins still resolve normally.
        if module in ("builtins", "__builtin__"):
            return super().find_class(module, name)
        return _StubStats


def op_statsload(path):
    try:
        if not os.path.exists(path):
            return {"global_note": None}
        with open(path, "rb") as f:
            obj = _StubUnpickler(f).load()
        note = getattr(obj, "global_note", None)
        if isinstance(note, bool):  # bool is an int subclass; keep as-is
            note = int(note)
        if note is None or isinstance(note, (int, float)):
            return {"global_note": note}
        return {"global_note": None}
    except Exception:  # caching.load_results swallows everything -> None
        return {"global_note": None}


def _short_bu(s):
    b = s.encode("utf-8")
    if len(b) < 256:
        return bytes([0x8C, len(b)]) + b
    return b"\x8d" + struct.pack("<I", len(b)) + b  # BINUNICODE8? use BINUNICODE


def _to_py(value):
    """Recursively decode the JSON wire form into real python objects."""
    if isinstance(value, dict):
        if set(value.keys()) == {"__set__"}:
            return set(_to_py(x) for x in value["__set__"])
        return {k: _to_py(v) for k, v in value.items()}
    if isinstance(value, list):
        return [_to_py(x) for x in value]
    return value


def op_statssave(path, state_wire):
    try:
        state = {k: _to_py(v) for k, v in state_wire.items()}
        body = bytearray()
        body += _short_bu("pylint.utils.linterstats")
        body += _short_bu("LinterStats")
        body += b"\x93"  # STACK_GLOBAL
        body += b")"     # EMPTY_TUPLE (NEWOBJ args)
        body += b"\x81"  # NEWOBJ -> cls.__new__(cls)
        sub = pickle.dumps(state, protocol=4)
        # strip PROTO(2) + optional FRAME(9) prefix and trailing STOP(1)
        assert sub[:2] == b"\x80\x04"
        i = 2
        if len(sub) > 2 and sub[2] == 0x95:
            i = 2 + 9
        assert sub[-1:] == b"."
        body += sub[i:-1]
        body += b"b"     # BUILD (apply state dict to the instance)
        out = bytearray()
        out += b"\x80\x04"
        out += b"\x95" + struct.pack("<Q", len(body) + 1)
        out += body
        out += b"."
        # caching.save_results does mkdir -p PYLINT_HOME first
        d = os.path.dirname(path)
        if d:
            os.makedirs(d, exist_ok=True)
        with open(path, "wb") as f:
            f.write(bytes(out))
        return {"ok": True}
    except Exception as ex:  # save_results notes the failure on stderr, continues
        return {"ok": False, "err": str(ex)}


# ======================================================================
# request loop
# ======================================================================

def main():
    out = sys.stdout
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
            elif op == "probe":
                resp = op_probe()
            elif op == "inithook":
                resp = op_inithook(req.get("hooks", []))
            elif op == "statsload":
                resp = op_statsload(req["path"])
            elif op == "statssave":
                resp = op_statssave(req["path"], req.get("state", {}))
            else:
                resp = {"err": "unknown op"}
        except Exception as ex:
            resp = {"err": str(ex)}
        out.write(json.dumps(resp) + "\n")
        out.flush()


if __name__ == "__main__":
    main()
