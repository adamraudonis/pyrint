"""Stdlib-only exact-E0001 oracle: replicates pylint's get_ast() error paths
for files the Rust parser rejects, WITHOUT importing astroid -- the released
binary must run on machines that have only a bare python3.

The wrapped str(SyntaxError) IS the message: astroid only wraps the original
exception (AstroidSyntaxError.error) and pylint embeds str(ex.error), so a
plain ast.parse with astroid's exact call shape reproduces E0001 verbatim.

Mirrored line-by-line from the pinned astroid 4.0.4 / pylint 4.0.5 sources:
  pylint/lint/pylinter.py:998-1038   get_ast except arms (E0001/F0010/F0002)
  astroid/manager.py:131-168         ast_from_file (get_source_file redirect)
  astroid/modutils.py:480-504        get_source_file(include_no_ext=True,
                                     prefer_stubs=False)
  astroid/builder.py:49-55           open_source_file
  astroid/builder.py:113-149         file_build except arms (encoding paths)
  astroid/builder.py:180-196         _data_build except tuple
  astroid/builder.py:486-505         _parse_string (data+'\n', filename=modname
                                     iff truthy, type-comment retry WITHOUT a
                                     filename -> '(<unknown>, line N)')
  pylint/utils/utils.py:151-154      tokenize_module (tokenize-form E0001)

Protocol (same as the original harness/syntax_oracle.py):
  JSON lines on stdin:  {"path": ..., "modname": ...}
  JSON lines on stdout, one of:
    {"ok": true}                                  -- builds fine
    {"ok": true, "tokenize": {"line": N, "col": N, "msg": "..."}}
        -- builds, but pylint's utils.tokenize_module would raise
           tokenize.TokenError (tokenize-form E0001: args[0] verbatim,
           line/col from args[1]; pylinter.py:1080-1090)
    {"kind": "syntax-error", "line": N, "offset": N|null, "msg": "..."}
        -- msg is the full E0001 args: "Parsing failed: '<...>'"
    {"kind": "parse-error", "msg": "..."}          -- F0010 args
    {"kind": "astroid-error"}                      -- F0002 path
"""

import ast
import json
import os
import re
import sys
import tokenize
import warnings

# astroid/builder.py module scope (PY312_PLUS / PY314_PLUS filters)
if sys.version_info >= (3, 12):
    warnings.filterwarnings("ignore", ".*invalid escape sequence", SyntaxWarning)
if sys.version_info >= (3, 14):
    warnings.filterwarnings(
        "ignore", "'(return|continue|break)' in a 'finally'", SyntaxWarning
    )

# astroid/modutils.py:41-49
if sys.platform.startswith("win"):
    PY_SOURCE_EXTS = ("py", "pyw", "pyi")
else:
    PY_SOURCE_EXTS = ("py", "pyi")


class NoSourceFile(Exception):
    pass


def get_source_file(filename):
    # modutils.get_source_file(filename, include_no_ext=True,
    # prefer_stubs=False); _path_from_filename is the identity off Jython.
    filename = os.path.abspath(filename)
    base, orig_ext = os.path.splitext(filename)
    orig_ext = orig_ext.lstrip(".")
    if orig_ext not in PY_SOURCE_EXTS and os.path.exists(f"{base}.{orig_ext}"):
        return f"{base}.{orig_ext}"
    for ext in PY_SOURCE_EXTS:
        source_path = f"{base}.{ext}"
        if os.path.exists(source_path):
            return source_path
    if not orig_ext and os.path.exists(base):
        return base
    raise NoSourceFile(filename)


def open_source_file(filename):
    # builder.open_source_file: detect_encoding sees the open file object, so
    # its SyntaxError embeds the ABSOLUTE path ("unknown encoding for '...'").
    with open(filename, "rb") as byte_stream:
        encoding = tokenize.detect_encoding(byte_stream.readline)[0]
    stream = open(filename, newline=None, encoding=encoding)
    data = stream.read()
    return stream, encoding, data


def parse_string(data, modname):
    # builder._parse_string via _ast.ParserModule.parse: filename is passed
    # only when truthy (ast.parse default is '<unknown>').
    try:
        if modname:
            return ast.parse(data + "\n", filename=modname, type_comments=True)
        return ast.parse(data + "\n", type_comments=True)
    except SyntaxError as exc:
        # Misplaced type annotations: retry without type-comment support.
        # The retry omits the filename, so a second failure renders
        # '(<unknown>, line N)'.
        if not re.search(r"#\s+type:", exc.text or ""):
            raise
        return ast.parse(data + "\n", type_comments=False)


# Leaf markers astroid maps via tables instead of visiting (rebuilder
# REDIRECT/op-class dicts): they must not extend the emulated visit path.
_REBUILD_SKIP = (ast.expr_context, ast.operator, ast.boolop, ast.unaryop, ast.cmpop)

# Node types whose children astroid visits through one EXTRA frame: the
# shared _visit_for/_visit_with/_visit_functiondef helpers, and Dict, whose
# items are rebuilt while the _visit_dict_items generator frame is live.
_REBUILD_3FRAME = (
    ast.FunctionDef,
    ast.AsyncFunctionDef,
    ast.For,
    ast.AsyncFor,
    ast.With,
    ast.AsyncWith,
    ast.Dict,
)


def _rebuild_emulate(module):
    # One shim frame aligns this walk's crash boundary with the reference
    # (astroid's rebuild starts deeper in its own call chain): pinned astroid
    # 4.0.4 / CPython 3.12 first crashes at 494 nested binop / unaryop /
    # attribute / ifexp / lambda levels in the oracle context (uncalibrated,
    # this walk first crashed at 495); harness/check_oracle.py re-verifies
    # both sides of the boundary differentially.
    _rebuild_shim(module)


def _rebuild_shim(module):
    _rebuild_frames(module)


def _rebuild_frames(node):
    # Frame-faithful stand-in for astroid's TreeRebuilder recursion: visit()
    # dispatch + visit_<type>() = 2 python frames per AST nesting level (3
    # for the _REBUILD_3FRAME types), so deep UNPARENTHESIZED expression
    # chains (binop / unaryop / attribute -- parenthesized nesting is
    # already rejected by CPython's parser at ~200) exhaust the default
    # sys.recursionlimit exactly like astroid's rebuild: RecursionError
    # escapes -> pylint get_ast except Exception -> F0002.
    _rebuild_frames_dispatch(node)


def _rebuild_frames_extra(node):
    _rebuild_frames_extra2(node)


def _rebuild_frames_extra2(node):
    _rebuild_frames_dispatch(node)


def _rebuild_frames_dispatch(node):
    for child in ast.iter_child_nodes(node):
        if isinstance(child, _REBUILD_3FRAME):
            _rebuild_frames_extra(child)
        elif not isinstance(child, _REBUILD_SKIP):
            _rebuild_frames(child)


def syntax_error_verdict(exc):
    # pylint get_ast AstroidSyntaxError arm (pylinter.py:1016-1026):
    # ValueError (null bytes) / TypeError / MemoryError have no lineno -> 0.
    line = getattr(exc, "lineno", None)
    if line is None:
        line = 0
    return {
        "kind": "syntax-error",
        "line": line,
        "offset": getattr(exc, "offset", None),
        "msg": f"Parsing failed: '{exc}'",
    }


def handle(path, modname):
    try:
        # manager.ast_from_file(source=True): one get_source_file redirect
        # (a .pyi argument resolves to a sibling .py; missing files keep the
        # original path and fail in open below).
        try:
            path = get_source_file(path)
        except NoSourceFile:
            pass
        # builder.file_build except arms, in source order
        try:
            stream, _encoding, data = open_source_file(path)
        except OSError as exc:
            # AstroidBuildingError("Unable to load file {path}:\n{error}")
            return {
                "kind": "parse-error",
                "msg": f"Unable to load file {path}:\n{exc}",
            }
        except (SyntaxError, LookupError) as exc:
            # AstroidSyntaxError(...): pylint embeds ex.error, i.e. exc itself
            return syntax_error_verdict(exc)
        except UnicodeError:
            # AstroidBuildingError("Wrong or no encoding specified for
            # {filename}.")
            return {
                "kind": "parse-error",
                "msg": f"Wrong or no encoding specified for {path}.",
            }
        with stream:
            # builder._data_build except tuple around _parse_string
            try:
                module = parse_string(data, modname)
            except (TypeError, ValueError, SyntaxError, MemoryError) as exc:
                return syntax_error_verdict(exc)
            # astroid's rebuild pass: RecursionError on deep trees is NOT in
            # the tuple above; it reaches the outer handler -> astroid-error
            _rebuild_emulate(module)
    except Exception:  # noqa: BLE001 -- pylint get_ast except Exception -> F0002
        return {"kind": "astroid-error"}
    # pylint utils.tokenize_module over module.stream() == open(file, "rb")
    try:
        with open(path, "rb") as byte_stream:
            list(tokenize.tokenize(byte_stream.readline))
    except tokenize.TokenError as ex:
        return {
            "ok": True,
            "tokenize": {
                "line": ex.args[1][0],
                "col": ex.args[1][1],
                "msg": ex.args[0],
            },
        }
    except Exception:  # noqa: BLE001
        pass  # other tokenize crashes take pylint's F0002 path; out of scope
    return {"ok": True}


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        req = json.loads(line)
        try:
            res = handle(req["path"], req["modname"])
        except Exception:  # noqa: BLE001 -- never kill the coprocess: any
            res = {"kind": "astroid-error"}  # unexpected crash is F0002
        sys.stdout.write(json.dumps(res) + "\n")
        sys.stdout.flush()


if __name__ == "__main__":
    main()
