//! ImportsChecker — in-scope subset (pylint/checkers/imports.py):
//!  - E0001 "Cannot import %r due to %s" via _get_imported_module
//!    (imports.py:1023-1053, AstroidSyntaxError branch at :1032-1036)
//!  - E0402 relative-beyond-top-level (TooManyLevelsError branch :1028-1031)
//!  - C0411 wrong-import-order / C0412 ungrouped-imports / C0413
//!    wrong-import-position computations: disabled under the target flags
//!    but their add_message/add_ignored_message calls feed the I0021
//!    useless-suppression bookkeeping (and inline `enable=` resurrection).
//!
//! isort.place_module is replicated with the py3-union stdlib set; the
//! FIRSTPARTY (src-path) arm is merged into THIRDPARTY — both arms perform
//! identical recording under the flow we port.

use pyast::tree::NodeKind;
use pyinfer::graph::BuildFail;
use pyinfer::value::GNode;

use crate::ckutils as u;
use crate::walker::WalkCx;

/// isort.stdlibs.py3.stdlib (union list; isort default py_version "py3")
static ISORT_STDLIB: &[&str] = &[
    "_ast", "_dummy_thread", "_thread", "abc", "aifc", "annotationlib", "antigravity",
    "argparse", "array", "ast", "asynchat", "asyncio", "asyncore", "atexit", "audioop",
    "base64", "bdb", "binascii", "binhex", "bisect", "builtins", "bz2", "cProfile",
    "calendar", "cgi", "cgitb", "chunk", "cmath", "cmd", "code", "codecs", "codeop",
    "collections", "colorsys", "compileall", "compression", "concurrent", "configparser",
    "contextlib", "contextvars", "copy", "copyreg", "crypt", "csv", "ctypes", "curses",
    "dataclasses", "datetime", "dbm", "decimal", "difflib", "dis", "distutils",
    "doctest", "dummy_threading", "email", "encodings", "ensurepip", "enum", "errno",
    "faulthandler", "fcntl", "filecmp", "fileinput", "fnmatch", "formatter", "fpectl",
    "fractions", "ftplib", "functools", "gc", "genericpath", "getopt", "getpass",
    "gettext", "glob", "graphlib", "grp", "gzip", "hashlib", "heapq", "hmac", "html",
    "http", "idlelib", "imaplib", "imghdr", "imp", "importlib", "inspect", "io",
    "ipaddress", "itertools", "json", "keyword", "lib2to3", "linecache", "locale",
    "logging", "lzma", "macpath", "mailbox", "mailcap", "marshal", "math", "mimetypes",
    "mmap", "modulefinder", "msilib", "msvcrt", "multiprocessing", "netrc", "nis",
    "nntplib", "nt", "ntpath", "nturl2path", "numbers", "opcode", "operator",
    "optparse", "os", "ossaudiodev", "parser", "pathlib", "pdb", "pickle",
    "pickletools", "pipes", "pkgutil", "platform", "plistlib", "poplib", "posix",
    "posixpath", "pprint", "profile", "pstats", "pty", "pwd", "py_compile", "pyclbr",
    "pydoc", "pydoc_data", "pyexpat", "queue", "quopri", "random", "re", "readline",
    "reprlib", "resource", "rlcompleter", "runpy", "sched", "secrets", "select",
    "selectors", "shelve", "shlex", "shutil", "signal", "site", "smtpd", "smtplib",
    "sndhdr", "socket", "socketserver", "spwd", "sqlite3", "sre", "sre_compile",
    "sre_constants", "sre_parse", "ssl", "stat", "statistics", "string", "stringprep",
    "struct", "subprocess", "sunau", "symbol", "symtable", "sys", "sysconfig",
    "syslog", "tabnanny", "tarfile", "telnetlib", "tempfile", "termios", "test",
    "textwrap", "this", "threading", "time", "timeit", "tkinter", "token", "tokenize",
    "tomllib", "trace", "traceback", "tracemalloc", "tty", "turtle", "turtledemo",
    "types", "typing", "unicodedata", "unittest", "urllib", "uu", "uuid", "venv",
    "warnings", "wave", "weakref", "webbrowser", "winreg", "winsound", "wsgiref",
    "xdrlib", "xml", "xmlrpc", "xx", "xxlimited", "xxlimited_35", "xxsubtype",
    "zipapp", "zipfile", "zipimport", "zlib", "zoneinfo",
];

const MAX_NUMBER_OF_IMPORT_SHOWN: usize = 6;

#[derive(Clone, Copy, PartialEq)]
enum Category {
    Future,
    Stdlib,
    Other, // THIRDPARTY / FIRSTPARTY (identical recording flow here)
    LocalFolder,
}

/// isort.place_module(package) with pylint's default isort config.
/// FIRSTPARTY (src-path probing) folds into Other.
fn place_module(package: &str) -> Category {
    if package == "__future__" {
        return Category::Future;
    }
    if package.starts_with('.') {
        return Category::LocalFolder;
    }
    if ISORT_STDLIB.contains(&package) {
        return Category::Stdlib;
    }
    Category::Other
}

#[derive(Default)]
pub struct ImportsChecker {
    /// (import node, imported package name) per module, in visit order
    imports_stack: Vec<(GNode, String)>,
    first_non_import_node: Option<GNode>,
}

impl ImportsChecker {
    pub fn visit_module(&mut self, _cx: &mut WalkCx, _node: GNode) {
        // pylint resets these in leave_module; visit_module only stores
        // _current_module_package. Reset here too for crash-safety.
        self.imports_stack.clear();
        self.first_non_import_node = None;
    }

    /// visit_import (imports.py:528-551), in-scope parts only.
    pub fn visit_import(&mut self, cx: &mut WalkCx, node: GNode) {
        let md = cx.eng.md(node.m);
        let names: Vec<String> = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Import { names } => {
                names.iter().map(|(n, _)| md.tree.s(*n).to_string()).collect()
            }
            _ => return,
        };
        drop(md);
        for name in &names {
            let imported = self.get_imported_module(cx, node, name);
            if cx
                .eng
                .parent(node)
                .map(|p| cx.eng.kind_is(p, |k| matches!(k, NodeKind::Module(_))))
                .unwrap_or(false)
            {
                self.check_position(cx, node);
            }
            if cx
                .eng
                .kind_is(cx.eng.scope(node), |k| matches!(k, NodeKind::Module(_)))
            {
                self.record_import(cx, node, imported, false);
            }
        }
    }

    /// visit_importfrom (imports.py:553-579), in-scope parts only.
    pub fn visit_importfrom(&mut self, cx: &mut WalkCx, node: GNode) {
        let md = cx.eng.md(node.m);
        let basename = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::ImportFrom { modname, .. } => md.tree.s(*modname).to_string(),
            _ => return,
        };
        drop(md);
        let imported = self.get_imported_module(cx, node, &basename);
        if cx
            .eng
            .parent(node)
            .map(|p| cx.eng.kind_is(p, |k| matches!(k, NodeKind::Module(_))))
            .unwrap_or(false)
        {
            self.check_position(cx, node);
        }
        if cx
            .eng
            .kind_is(cx.eng.scope(node), |k| matches!(k, NodeKind::Module(_)))
        {
            self.record_import(cx, node, imported, true);
        }
    }

    /// compute_first_non_import_node (imports.py:612-648)
    pub fn compute_first_non_import_node(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        if self.first_non_import_node.is_some() {
            return;
        }
        let Some(parent) = eng.parent(node) else { return };
        if !eng.kind_is(parent, |k| matches!(k, NodeKind::Module(_))) {
            return;
        }
        if eng.kind_is(node, |k| matches!(k, NodeKind::Try(_) | NodeKind::TryStar(_))) {
            let has_imports = !crate::basicerr::nodes_of_class(
                eng,
                node,
                |k| matches!(k, NodeKind::Import { .. } | NodeKind::ImportFrom { .. }),
                |_| false,
            )
            .is_empty();
            if has_imports {
                return;
            }
        }
        if eng.kind_is(node, |k| matches!(k, NodeKind::Assign { .. })) {
            let md = eng.md(node.m);
            if let NodeKind::Assign { targets, .. } = &md.tree.nodes[node.n.idx()].kind {
                let all_dunder = targets.iter().all(|&t| match &md.tree.nodes[t.idx()].kind {
                    NodeKind::AssignName { name } => {
                        let n = md.tree.s(*name);
                        n.starts_with("__") && n.ends_with("__")
                    }
                    _ => false,
                });
                if all_dunder {
                    return;
                }
            }
        }
        self.first_non_import_node = Some(node);
    }

    /// visit_functiondef = visit_classdef = visit_for = visit_while
    /// (imports.py:650-672)
    pub fn visit_functiondef_family(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        if self.first_non_import_node.is_some() {
            return;
        }
        let Some(parent) = eng.parent(node) else { return };
        if !eng.kind_is(eng.scope(parent), |k| matches!(k, NodeKind::Module(_))) {
            return;
        }
        let mut root = node;
        loop {
            let Some(p) = eng.parent(root) else { break };
            if eng.kind_is(p, |k| matches!(k, NodeKind::Module(_))) {
                break;
            }
            root = p;
        }
        if eng.kind_is(root, |k| {
            matches!(k, NodeKind::If { .. } | NodeKind::Try(_) | NodeKind::TryStar(_))
        }) {
            let has_imports = !crate::basicerr::nodes_of_class(
                eng,
                root,
                |k| matches!(k, NodeKind::Import { .. } | NodeKind::ImportFrom { .. }),
                |_| false,
            )
            .is_empty();
            if has_imports {
                return;
            }
        }
        self.first_non_import_node = Some(node);
    }

    /// _check_position (imports.py:698-715) — C0413 + attempt recording
    fn check_position(&mut self, cx: &mut WalkCx, node: GNode) {
        let Some(first) = self.first_non_import_node else { return };
        let first_line = u::lineno(cx.eng, first);
        if (cx.is_enabled)("C0413", first_line) {
            let text = u::format_template(
                "Import \"%s\" should be placed at the top of the module",
                &[&u::as_string(cx.eng, node)],
            );
            cx.emit_node(
                "C0413",
                u::lineno(cx.eng, node),
                u::col_offset(cx.eng, node) as i64,
                text,
            );
        } else {
            (cx.add_ignored)("C0413", u::lineno(cx.eng, node));
        }
    }

    /// _record_import (imports.py:717-742)
    fn record_import(
        &mut self,
        cx: &mut WalkCx,
        node: GNode,
        imported: Option<pyinfer::value::ModId>,
        is_from: bool,
    ) {
        let eng = cx.eng;
        let md = eng.md(node.m);
        let mut importedname: Option<String> = if is_from {
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::ImportFrom { modname, .. } => {
                    let s = md.tree.s(*modname).to_string();
                    if s.is_empty() {
                        None
                    } else {
                        Some(s)
                    }
                }
                _ => None,
            }
        } else {
            imported.map(|m| eng.md(m).name.clone())
        };
        if importedname.is_none() {
            let first = match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::Import { names } | NodeKind::ImportFrom { names, .. } => names
                    .first()
                    .map(|(n, _)| md.tree.s(*n).split('.').next().unwrap_or("").to_string()),
                _ => None,
            };
            importedname = first;
        }
        let mut importedname = importedname.unwrap_or_default();
        let level: Option<u32> = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::ImportFrom { level, .. } => *level,
            _ => None,
        };
        drop(md);
        if is_from && level.map(|l| l >= 1).unwrap_or(false) {
            importedname = format!(".{importedname}");
        }
        self.imports_stack.push((node, importedname));
    }

    /// leave_module (imports.py:581-610) — C0411/C0412 + attempt recording
    pub fn leave_module(&mut self, cx: &mut WalkCx, _node: GNode) {
        let eng = cx.eng;
        // ---- _check_imports_order (imports.py:764-870) ----
        let mut std_imports: Vec<(GNode, String)> = Vec::new();
        let mut external_imports: Vec<(GNode, String)> = Vec::new();
        let mut local_imports: Vec<(GNode, String)> = Vec::new();
        let mut third_party_not_ignored: Vec<(GNode, String)> = Vec::new();
        let first_party_not_ignored: Vec<(GNode, String)> = Vec::new();
        let mut local_not_ignored: Vec<(GNode, String)> = Vec::new();
        let stack = std::mem::take(&mut self.imports_stack);
        for (node, modname) in &stack {
            let package = if modname.starts_with('.') {
                let second = modname.split('.').nth(1).unwrap_or("");
                format!(".{second}")
            } else {
                modname.split('.').next().unwrap_or("").to_string()
            };
            let nested = !eng
                .parent(*node)
                .map(|p| eng.kind_is(p, |k| matches!(k, NodeKind::Module(_))))
                .unwrap_or(false);
            let ignore_for_import_order = !(cx.is_enabled)("C0411", u::lineno(eng, *node));
            let category = place_module(&package);
            let entry = (*node, package.clone());
            match category {
                Category::Future | Category::Stdlib => {
                    std_imports.push(entry.clone());
                    let wrong = !third_party_not_ignored.is_empty()
                        || !first_party_not_ignored.is_empty()
                        || !local_not_ignored.is_empty();
                    if wrong {
                        // _is_fallback_import: are_exclusive vs not-ignored
                        let all: Vec<GNode> = third_party_not_ignored
                            .iter()
                            .chain(first_party_not_ignored.iter())
                            .chain(local_not_ignored.iter())
                            .map(|(n, _)| *n)
                            .collect();
                        if all.iter().any(|&i| u::are_exclusive(eng, i, *node)) {
                            continue;
                        }
                        if !nested {
                            let what =
                                format!("standard import \"{}\"", full_import_name(eng, &entry));
                            let order = out_of_order_string(
                                eng,
                                &third_party_not_ignored,
                                &first_party_not_ignored,
                                &local_not_ignored,
                            );
                            cx.emit_node(
                                "C0411",
                                u::lineno(eng, *node),
                                u::col_offset(eng, *node) as i64,
                                u::format_template(
                                    "%s should be placed before %s",
                                    &[&what, &order],
                                ),
                            );
                        }
                    }
                }
                Category::Other => {
                    external_imports.push(entry.clone());
                    if !nested {
                        if !ignore_for_import_order {
                            third_party_not_ignored.push(entry.clone());
                        } else {
                            (cx.add_ignored)("C0411", u::lineno(eng, *node));
                        }
                    }
                    let wrong =
                        !first_party_not_ignored.is_empty() || !local_not_ignored.is_empty();
                    if wrong && !nested {
                        let what =
                            format!("third party import \"{}\"", full_import_name(eng, &entry));
                        let order = out_of_order_string(
                            eng,
                            &[],
                            &first_party_not_ignored,
                            &local_not_ignored,
                        );
                        cx.emit_node(
                            "C0411",
                            u::lineno(eng, *node),
                            u::col_offset(eng, *node) as i64,
                            u::format_template("%s should be placed before %s", &[&what, &order]),
                        );
                    }
                }
                Category::LocalFolder => {
                    local_imports.push(entry.clone());
                    if !nested {
                        if !ignore_for_import_order {
                            local_not_ignored.push(entry.clone());
                        } else {
                            (cx.add_ignored)("C0411", u::lineno(eng, *node));
                        }
                    }
                }
            }
        }
        // ---- grouped-by-package loop (C0412, imports.py:585-608) ----
        let mut met_import: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut met_from: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut current_package: Option<String> = None;
        for (import_node, import_name) in std_imports
            .iter()
            .chain(external_imports.iter())
            .chain(local_imports.iter())
        {
            let is_from = eng.kind_is(*import_node, |k| matches!(k, NodeKind::ImportFrom { .. }));
            let met = if is_from { &mut met_from } else { &mut met_import };
            let package = import_name.split('.').next().unwrap_or("").to_string();
            if current_package.is_some()
                && current_package.as_deref() != Some(package.as_str())
                && met.contains(&package)
                && !u::in_type_checking_block(eng, cx.caches, *import_node)
                && !eng
                    .parent(*import_node)
                    .map(|p| u::is_if(eng, p) && u::is_sys_guard(eng, p))
                    .unwrap_or(false)
            {
                cx.emit_node(
                    "C0412",
                    u::lineno(eng, *import_node),
                    u::col_offset(eng, *import_node) as i64,
                    u::format_template("Imports from package %s are not grouped", &[&package]),
                );
            }
            current_package = Some(package.clone());
            if !(cx.is_enabled)("C0412", u::lineno(eng, *import_node)) {
                continue;
            }
            met.insert(package);
        }
        self.imports_stack = Vec::new();
        self.first_non_import_node = None;
    }

    /// _get_imported_module (imports.py:1023-1053).
    fn get_imported_module(
        &mut self,
        cx: &mut WalkCx,
        importnode: GNode,
        modname: &str,
    ) -> Option<pyinfer::value::ModId> {
        match cx.eng.do_import_module(importnode, Some(modname)) {
            Ok(id) => Some(id),
            Err(BuildFail::TooManyLevels) => {
                // astroid.TooManyLevelsError (imports.py:1028-1031)
                if !ignore_import_failure(cx, importnode, modname) {
                    let line = u::lineno(cx.eng, importnode);
                    let col = u::col_offset(cx.eng, importnode).max(0) as i64;
                    cx.emit_node(
                        "E0402",
                        line,
                        col,
                        "Attempted relative import beyond top-level package".to_string(),
                    );
                }
                None
            }
            Err(BuildFail::Syntax { path, modname: resolved, .. }) => {
                // astroid.AstroidSyntaxError (imports.py:1032-1036):
                //   message = f"Cannot import {modname!r} due to '{exc.error}'"
                //   add_message("syntax-error", line=importnode.lineno, ...)
                // exc.error is the original SyntaxError; its str() embeds the
                // modname astroid resolved. The exact CPython text comes from
                // the persistent oracle keyed by (path, resolved name); a
                // None verdict means astroid would NOT raise (ruff/CPython
                // acceptance mismatch) -> emit nothing.
                if let Some(errstr) = (cx.import_oracle)(&path, &resolved) {
                    let line = u::lineno(cx.eng, importnode);
                    let text = format!(
                        "Cannot import {} due to '{}'",
                        u::py_repr_str(modname),
                        errstr
                    );
                    // node-less message: col_offset None -> 0
                    cx.emit_nodeless("E0001", line, 0, text);
                }
                None
            }
            // AstroidBuildingError branch: import-error (E0401) is disabled
            // under the target flags -> `if not is_message_enabled(...)
            // return None` fires first (imports.py:1039-1040)
            Err(BuildFail::Import(_)) => None,
            // astroid-crash build: pylint's RecursionError is NOT caught by
            // _get_imported_module — it aborts the module check (F0002).
            // The engine already tripped the crash flag; emit nothing.
            Err(BuildFail::Crash) => None,
        }
    }
}

/// _get_full_import_name (imports.py:998-1021)
fn full_import_name(eng: &pyinfer::graph::Engine, entry: &(GNode, String)) -> String {
    let (node, package) = entry;
    let md = eng.md(node.m);
    match &md.tree.nodes[node.n.idx()].kind {
        NodeKind::ImportFrom { modname, names, .. } => {
            let m = md.tree.s(*modname).to_string();
            let n = names
                .first()
                .map(|(n, _)| md.tree.s(*n).to_string())
                .unwrap_or_default();
            format!("{m}.{n}")
        }
        NodeKind::Import { names } => {
            let n = names
                .first()
                .map(|(n, _)| md.tree.s(*n).to_string())
                .unwrap_or_default();
            if n.split('.').next() == Some(package.as_str()) {
                n
            } else {
                package.clone()
            }
        }
        _ => package.clone(),
    }
}

/// _get_out_of_order_string (imports.py:871-996)
fn out_of_order_string(
    eng: &pyinfer::graph::Engine,
    third_party: &[(GNode, String)],
    first_party: &[(GNode, String)],
    local: &[(GNode, String)],
) -> String {
    let section = |imports: &[(GNode, String)], label: &str| -> String {
        if imports.is_empty() {
            return String::new();
        }
        let plural = if imports.len() > 1 { "s" } else { "" };
        let render = |items: &[(GNode, String)]| -> String {
            items
                .iter()
                .map(|e| format!("\"{}\"", full_import_name(eng, e)))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let imports_list = if imports.len() > MAX_NUMBER_OF_IMPORT_SHOWN {
            format!(
                "{} (...) {}",
                render(&imports[..MAX_NUMBER_OF_IMPORT_SHOWN / 2]),
                render(&imports[imports.len() - MAX_NUMBER_OF_IMPORT_SHOWN / 2..])
            )
        } else {
            render(imports)
        };
        format!("{label} import{plural} {imports_list}")
    };
    let third = section(third_party, "third party");
    let first = section(first_party, "first party");
    let loc = section(local, "local");
    let delimiter_third = if !third.is_empty() {
        if !first.is_empty() && !loc.is_empty() {
            ", "
        } else if !first.is_empty() || !loc.is_empty() {
            " and "
        } else {
            ""
        }
    } else {
        ""
    };
    let d1 = if !first.is_empty() {
        if !third.is_empty() && !loc.is_empty() {
            ", "
        } else {
            " "
        }
    } else {
        ""
    };
    let d2 = if !first.is_empty() && !loc.is_empty() { "and " } else { "" };
    format!("{third}{delimiter_third}{first}{d1}{d2}{loc}")
}

/// _ignore_import_failure (imports.py:140-155). ignored-modules default ().
fn ignore_import_failure(cx: &mut WalkCx, node: GNode, _modname: &str) -> bool {
    // is_module_ignored(modname, ()) -> False with the default config
    if u::in_type_checking_block(cx.eng, cx.caches, node) {
        return true;
    }
    if let Some(parent) = cx.eng.parent(node) {
        if u::is_if(cx.eng, parent) && u::is_sys_guard(cx.eng, parent) {
            return true;
        }
    }
    u::node_ignores_exception(cx.eng, cx.caches, node, "ImportError")
}
