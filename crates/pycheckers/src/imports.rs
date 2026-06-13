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
    ThirdParty,
    FirstParty,
    LocalFolder,
}

/// isort 8.0.1 exists_case_sensitive (utils.py): os.path.exists + the exact
/// name appears in the parent directory listing
fn exists_case_sensitive(path: &std::path::Path) -> bool {
    if !path.exists() {
        return false;
    }
    match (path.parent(), path.file_name()) {
        (Some(dir), Some(name)) => std::fs::read_dir(dir)
            .map(|rd| rd.flatten().any(|e| e.file_name() == name))
            .unwrap_or(false),
        _ => true,
    }
}

/// isort place._is_module
fn isort_is_module(path: &std::path::Path) -> bool {
    const EXT_SUFFIXES: &[&str] = &[".cpython-312-darwin.so", ".abi3.so", ".so"];
    let base = path.to_path_buf();
    let with_ext = |ext: &str| {
        let mut p = base.clone();
        p.set_extension(ext.trim_start_matches('.'));
        p
    };
    if exists_case_sensitive(&with_ext("py")) {
        return true;
    }
    for ext in EXT_SUFFIXES {
        // Path.with_suffix replaces from the LAST dot; module names here are
        // single components, so appending is equivalent
        let p = std::path::PathBuf::from(format!("{}{}", base.display(), ext));
        if exists_case_sensitive(&p) {
            return true;
        }
    }
    exists_case_sensitive(&base.join("__init__.py"))
}

/// isort.place_module(package) with pylint's isort.Config(
/// extra_standard_library=(), known_third_party=("enchant",)):
/// _local -> _known_pattern (FUTURE/STDLIB/THIRDPARTY) -> _src_path
/// (cwd/src, cwd) -> default THIRDPARTY (place.py, isort 8.0.1)
fn place_module(package: &str) -> Category {
    if package.starts_with('.') {
        return Category::LocalFolder;
    }
    if package == "__future__" {
        return Category::Future;
    }
    if ISORT_STDLIB.contains(&package) {
        return Category::Stdlib;
    }
    if package == "enchant" {
        return Category::ThirdParty;
    }
    // _src_path over (cwd/"src", cwd)
    if let Ok(cwd) = std::env::current_dir() {
        for src_path in [cwd.join("src"), cwd.clone()] {
            let mut module_path = src_path.join(package);
            if !module_path.is_dir()
                && src_path.file_name().map(|n| n == package).unwrap_or(false)
            {
                module_path = src_path.clone();
            }
            // nested_module is always empty here (single component)
            let src_path_is_module = src_path
                .file_name()
                .map(|n| n == package)
                .unwrap_or(false)
                && src_path.is_dir()
                && exists_case_sensitive(&src_path);
            if isort_is_module(&module_path)
                || (exists_case_sensitive(&module_path) && module_path.is_dir())
                || src_path_is_module
            {
                return Category::FirstParty;
            }
        }
    }
    Category::ThirdParty
}

#[derive(Default)]
pub struct ImportsChecker {
    /// (import node, imported package name) per module, in visit order
    imports_stack: Vec<(GNode, String)>,
    first_non_import_node: Option<GNode>,
    /// _current_module_package (visit_module, imports.py:524-526)
    current_module_package: bool,
    /// import_graph: context module -> imported modules (set; insertion
    /// order kept for the CPython set-order simulation). RUN-level state.
    pub import_graph: indexmap::IndexMap<String, Vec<String>>,
    /// _excluded_edges (pragma- or TYPE_CHECKING-suppressed edges)
    pub excluded_edges: rustc_hash::FxHashMap<String, rustc_hash::FxHashSet<String>>,
    /// _module_pkg bookkeeping (only feeds RP reports; kept for parity)
    module_pkg: rustc_hash::FxHashMap<String, String>,
    /// isort module_with_reason lru_cache(1000) equivalent
    place_cache: rustc_hash::FxHashMap<String, Category>,
}

impl ImportsChecker {
    pub fn visit_module(&mut self, cx: &mut WalkCx, node: GNode) {
        // pylint resets these in leave_module; visit_module only stores
        // _current_module_package. Reset here too for crash-safety.
        self.imports_stack.clear();
        self.first_non_import_node = None;
        self.current_module_package = {
            let md = cx.eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::Module(d) => d.package,
                _ => false,
            }
        };
    }

    /// visit_import (imports.py:528-551).
    pub fn visit_import(&mut self, cx: &mut WalkCx, node: GNode) {
        let md = cx.eng.md(node.m);
        let names: Vec<String> = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::Import { names } => {
                names.iter().map(|(n, _)| md.tree.s(*n).to_string()).collect()
            }
            _ => return,
        };
        drop(md);
        if cx.full {
            self.check_reimport(cx, node, None, None);
            self.check_import_as_rename(cx, node);
            self.check_toplevel(cx, node);
            if names.len() >= 2 {
                cx.emit_node(
                    "C0410",
                    u::lineno(cx.eng, node),
                    u::col_offset(cx.eng, node).max(0) as i64,
                    u::format_template("Multiple imports on one line (%s)", &[&names.join(", ")]),
                );
            }
        }
        for name in &names {
            // check_deprecated_module (mixin) — W4901, per name, BEFORE the
            // import resolution (imports.py:543-545)
            crate::deprecated::check_deprecated_module(cx, node, name);
            // preferred-modules default () — dead
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
            if cx.full {
                if let Some(mid) = imported {
                    let mname = cx.eng.md(mid).name.clone();
                    self.add_imported_module(cx, node, &mname);
                }
            }
        }
    }

    /// visit_importfrom (imports.py:553-579).
    pub fn visit_importfrom(&mut self, cx: &mut WalkCx, node: GNode) {
        let md = cx.eng.md(node.m);
        let (basename, level, names): (String, Option<u32>, Vec<(String, Option<String>)>) =
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::ImportFrom { modname, level, names } => (
                    md.tree.s(*modname).to_string(),
                    *level,
                    names
                        .iter()
                        .map(|&(n, a)| {
                            (md.tree.s(n).to_string(), a.map(|x| md.tree.s(x).to_string()))
                        })
                        .collect(),
                ),
                _ => return,
            };
        drop(md);
        let imported = self.get_imported_module(cx, node, &basename);
        // W4901 on the ABSOLUTE (relative-resolved) name (imports.py:561)
        let absolute_name = importfrom_absolute_name(cx.eng, node);
        if cx.full {
            self.check_import_as_rename(cx, node);
            self.check_misplaced_future(cx, node, &basename);
        }
        crate::deprecated::check_deprecated_module(cx, node, &absolute_name);
        if cx.full {
            // preferred-modules default () — dead
            self.check_wildcard_imports(cx, node, &basename, &names);
            self.check_same_line_imports(cx, node, &names);
            self.check_reimport(cx, node, Some(&basename), level);
            self.check_toplevel(cx, node);
        }
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
        if cx.full {
            if let Some(mid) = imported {
                let mname = cx.eng.md(mid).name.clone();
                for (name, _) in &names {
                    if name != "*" {
                        self.add_imported_module(cx, node, &format!("{mname}.{name}"));
                    } else {
                        self.add_imported_module(cx, node, &mname);
                    }
                }
            }
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
        let mut first_party_not_ignored: Vec<(GNode, String)> = Vec::new();
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
            let category = *self
                .place_cache
                .entry(package.clone())
                .or_insert_with(|| place_module(&package));
            let entry = (*node, package.clone());
            match category {
                Category::Future | Category::Stdlib => {
                    std_imports.push(entry.clone());
                    // wrong_import = FIRST non-empty list (python `or`)
                    let wrong: &[(GNode, String)] = if !third_party_not_ignored.is_empty() {
                        &third_party_not_ignored
                    } else if !first_party_not_ignored.is_empty() {
                        &first_party_not_ignored
                    } else {
                        &local_not_ignored
                    };
                    // _is_fallback_import over THAT list only
                    if wrong.iter().any(|(i, _)| u::are_exclusive(eng, *i, *node)) {
                        continue;
                    }
                    if !wrong.is_empty() && !nested {
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
                Category::ThirdParty => {
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
                Category::FirstParty => {
                    external_imports.push(entry.clone());
                    if !nested {
                        if !ignore_for_import_order {
                            first_party_not_ignored.push(entry.clone());
                        } else {
                            (cx.add_ignored)("C0411", u::lineno(eng, *node));
                        }
                    }
                    let wrong = !local_not_ignored.is_empty();
                    if wrong && !nested {
                        let what =
                            format!("first party import \"{}\"", full_import_name(eng, &entry));
                        let order =
                            out_of_order_string(eng, &[], &[], &local_not_ignored);
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

    /// _check_import_as_rename (imports.py:1120-1142) — C0414/R0402
    fn check_import_as_rename(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let names: Vec<(String, Option<String>)> = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::Import { names } | NodeKind::ImportFrom { names, .. } => names
                    .iter()
                    .map(|&(n, a)| {
                        (md.tree.s(n).to_string(), a.map(|x| md.tree.s(x).to_string()))
                    })
                    .collect(),
                _ => return,
            }
        };
        for (qname, alias) in &names {
            // `if not all(name): return` — no alias (or empty) STOPS the check
            let Some(alias) = alias else { return };
            if qname.is_empty() || alias.is_empty() {
                return;
            }
            let splitted: Vec<&str> = match qname.rsplitn(2, '.').collect::<Vec<_>>() {
                v if v.len() == 2 => vec![v[1], v[0]],
                v => vec![v[0]],
            };
            let import_name = *splitted.last().unwrap();
            if import_name != alias {
                continue;
            }
            if splitted.len() == 1 {
                // allow-reexport-from-package default False
                cx.emit_node(
                    "C0414",
                    u::lineno(eng, node),
                    u::col_offset(eng, node).max(0) as i64,
                    "Import alias does not rename original package".to_string(),
                );
            } else if splitted.len() == 2 {
                cx.emit_node(
                    "R0402",
                    u::lineno(eng, node),
                    u::col_offset(eng, node).max(0) as i64,
                    u::format_template(
                        "Use 'from %s import %s' instead",
                        &[splitted[0], import_name],
                    ),
                );
            }
        }
    }

    /// _check_misplaced_future (imports.py:678-688) — W0410
    fn check_misplaced_future(&mut self, cx: &mut WalkCx, node: GNode, basename: &str) {
        if basename != "__future__" {
            return;
        }
        let eng = cx.eng;
        let Some(prev) = u::previous_sibling(eng, node) else { return };
        let prev_is_future = {
            let md = eng.md(prev.m);
            matches!(&md.tree.nodes[prev.n.idx()].kind,
                NodeKind::ImportFrom { modname, .. } if md.tree.s(*modname) == "__future__")
        };
        if !prev_is_future {
            cx.emit_node(
                "W0410",
                u::lineno(eng, node),
                u::col_offset(eng, node).max(0) as i64,
                "__future__ import is not the first non docstring statement".to_string(),
            );
        }
    }

    /// _check_wildcard_imports (imports.py:1232-1249) — W0401
    fn check_wildcard_imports(
        &mut self,
        cx: &mut WalkCx,
        node: GNode,
        basename: &str,
        names: &[(String, Option<String>)],
    ) {
        let eng = cx.eng;
        // skip inside __init__.py (issue #2026)
        let is_pkg = {
            let md = eng.md(node.m);
            match &md.tree.nodes[pyast::NodeId::MODULE.idx()].kind {
                NodeKind::Module(d) => d.package,
                _ => false,
            }
        };
        if is_pkg {
            return;
        }
        // allow-wildcard-with-all default False -> never allowed
        for (name, _) in names {
            if name == "*" {
                cx.emit_node(
                    "W0401",
                    u::lineno(eng, node),
                    u::col_offset(eng, node).max(0) as i64,
                    u::format_template("Wildcard import %s", &[basename]),
                );
            }
        }
    }

    /// _check_same_line_imports (imports.py:690-696) — W0404 within one stmt
    fn check_same_line_imports(
        &mut self,
        cx: &mut WalkCx,
        node: GNode,
        names: &[(String, Option<String>)],
    ) {
        let mut counts: indexmap::IndexMap<&str, usize> = indexmap::IndexMap::new();
        for (name, _) in names {
            *counts.entry(name.as_str()).or_insert(0) += 1;
        }
        let line = u::lineno(cx.eng, node);
        for (name, count) in counts {
            if count > 1 {
                cx.emit_node(
                    "W0404",
                    line,
                    u::col_offset(cx.eng, node).max(0) as i64,
                    u::format_template(
                        "Reimport %r (imported line %s)",
                        &[name, &line.to_string()],
                    ),
                );
            }
        }
    }

    /// _check_reimport (imports.py:1144-1171) — W0404/W0416
    fn check_reimport(
        &mut self,
        cx: &mut WalkCx,
        node: GNode,
        basename: Option<&str>,
        level: Option<u32>,
    ) {
        if !(cx.cfg_enabled)("W0404") && !(cx.cfg_enabled)("W0416") {
            return;
        }
        let eng = cx.eng;
        let frame = eng.frame(node);
        let root = GNode { m: node.m, n: pyast::NodeId::MODULE };
        let mut contexts: Vec<(GNode, Option<u32>)> = vec![(frame, level)];
        if root != frame {
            contexts.push((root, None));
        }
        let names: Vec<(String, Option<String>)> = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::Import { names } | NodeKind::ImportFrom { names, .. } => names
                    .iter()
                    .map(|&(n, a)| {
                        (md.tree.s(n).to_string(), a.map(|x| md.tree.s(x).to_string()))
                    })
                    .collect(),
                _ => return,
            }
        };
        for (known_context, known_level) in &contexts {
            for (name, alias) in &names {
                let (first, msg) = get_first_import(
                    eng,
                    node,
                    *known_context,
                    name,
                    basename,
                    *known_level,
                    alias.as_deref(),
                );
                if let (Some(first), Some(msg)) = (first, msg) {
                    let display = if msg == "W0404" {
                        name.clone()
                    } else {
                        alias.clone().unwrap_or_default()
                    };
                    let template = if msg == "W0404" {
                        "Reimport %r (imported line %s)"
                    } else {
                        "Shadowed %r (imported line %s)"
                    };
                    let fline = eng.fromlineno(first);
                    cx.emit_node(
                        msg,
                        u::lineno(eng, node),
                        u::col_offset(eng, node).max(0) as i64,
                        u::format_template(template, &[&display, &fline.to_string()]),
                    );
                }
            }
        }
    }

    /// _check_toplevel (imports.py:1251-1275) — C0415
    fn check_toplevel(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        if u::is_module(eng, eng.scope(node)) {
            return;
        }
        let md = eng.md(node.m);
        let module_names: Vec<String> = match &md.tree.nodes[node.n.idx()].kind {
            NodeKind::ImportFrom { modname, names, .. } => {
                let m = md.tree.s(*modname).to_string();
                names
                    .iter()
                    .map(|&(n, _)| format!("{}.{}", m, md.tree.s(n)))
                    .collect()
            }
            NodeKind::Import { names } => {
                names.iter().map(|&(n, _)| md.tree.s(n).to_string()).collect()
            }
            _ => return,
        };
        drop(md);
        // allow-any-import-level default () -> all scoped
        if !module_names.is_empty() {
            cx.emit_node(
                "C0415",
                u::lineno(eng, node),
                u::col_offset(eng, node).max(0) as i64,
                u::format_template(
                    "Import outside toplevel (%s)",
                    &[&module_names.join(", ")],
                ),
            );
        }
    }

    /// _add_imported_module (imports.py:1055-1092) — W0406 + R0401 graph
    fn add_imported_module(&mut self, cx: &mut WalkCx, node: GNode, importedmodname: &str) {
        let eng = cx.eng;
        let (context_name, module_file) = {
            let md = eng.md(node.m);
            (md.name.clone(), md.file.clone())
        };
        let base_is_init = std::path::Path::new(&module_file)
            .file_stem()
            .map(|s| s == "__init__")
            .unwrap_or(false);
        let is_relative = {
            let md = eng.md(node.m);
            matches!(&md.tree.nodes[node.n.idx()].kind,
                NodeKind::ImportFrom { level: Some(l), .. } if *l > 0)
        };
        let importedmodname: String =
            match get_module_part(eng, importedmodname, is_relative) {
                Some(m) => m,
                None => importedmodname.to_string(), // ImportError -> pass
            };
        if context_name == importedmodname {
            cx.emit_node(
                "W0406",
                u::lineno(eng, node),
                u::col_offset(eng, node).max(0) as i64,
                "Module import itself".to_string(),
            );
        } else if !is_stdlib_module(eng, &importedmodname) {
            if !base_is_init && !self.module_pkg.contains_key(&context_name) {
                let pkg = match context_name.rsplitn(2, '.').nth(1) {
                    Some(p) => p.to_string(),
                    None => context_name.clone(),
                };
                self.module_pkg.insert(context_name.clone(), pkg);
            }
            let edges = self.import_graph.entry(context_name.clone()).or_default();
            if !edges.iter().any(|e| e == &importedmodname) {
                edges.push(importedmodname.clone());
            }
            if !(cx.is_enabled)("R0401", u::lineno(eng, node))
                || u::in_type_checking_block(eng, cx.caches, node)
            {
                let ex = self.excluded_edges.entry(context_name).or_default();
                ex.insert(importedmodname);
            }
        }
    }

    /// close() (imports.py:484-490) — R0401 cyclic-import. `emit` receives
    /// fully formatted messages (the caller owns module attribution).
    pub fn close_cycles(&self, emit: &mut dyn FnMut(String)) {
        // _import_graph_without_ignored_edges: deepcopy (re-insertion in
        // iteration order) then difference_update; survivors keep the
        // copy's slot order.
        let empty: rustc_hash::FxHashSet<String> = Default::default();
        let mut filtered: indexmap::IndexMap<&str, Vec<&str>> = indexmap::IndexMap::new();
        for (k, edges) in &self.import_graph {
            let refs: Vec<&str> = edges.iter().map(|e| e.as_str()).collect();
            // original set iteration order
            let order_a = crate::pyset::cpython_set_order(&refs);
            let o_a: Vec<&str> = order_a.iter().map(|&i| refs[i]).collect();
            // deepcopy: fresh table built in o_a insertion order
            let order_b = crate::pyset::cpython_set_order(&o_a);
            let excluded = self.excluded_edges.get(k.as_str()).unwrap_or(&empty);
            let survivors: Vec<&str> = order_b
                .iter()
                .map(|&i| o_a[i])
                .filter(|e: &&str| !excluded.contains(*e as &str))
                .collect();
            filtered.insert(k.as_str(), survivors);
        }
        // get_cycles (pylint/graph.py:164-211)
        let vertices: Vec<&str> = filtered.keys().copied().collect();
        let mut result: Vec<Vec<&str>> = Vec::new();
        fn get_cycles<'a>(
            graph: &indexmap::IndexMap<&'a str, Vec<&'a str>>,
            path: &mut Vec<&'a str>,
            visited: &mut rustc_hash::FxHashSet<&'a str>,
            result: &mut Vec<Vec<&'a str>>,
            vertice: &'a str,
        ) {
            if path.iter().any(|&p| p == vertice) {
                let mut cycle = vec![vertice];
                for &node in path.iter().rev() {
                    if node == vertice {
                        break;
                    }
                    cycle.insert(0, node);
                }
                let start_from = cycle.iter().min().copied().unwrap();
                let index = cycle.iter().position(|&c| c == start_from).unwrap();
                let rotated: Vec<&str> =
                    cycle[index..].iter().chain(cycle[..index].iter()).copied().collect();
                if !result.contains(&rotated) {
                    result.push(rotated);
                }
                return;
            }
            path.push(vertice);
            if let Some(neighbors) = graph.get(vertice) {
                for &node in neighbors {
                    if !visited.contains(node) {
                        get_cycles(graph, path, visited, result, node);
                        visited.insert(node);
                    }
                }
            }
            path.pop();
        }
        for &vertice in &vertices {
            let mut path = Vec::new();
            let mut visited = rustc_hash::FxHashSet::default();
            get_cycles(&filtered, &mut path, &mut visited, &mut result, vertice);
        }
        for cycle in result {
            emit(cycle.join(" -> "));
        }
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


/// utils.get_import_name (pylint utils.py:1820-1843): resolve a relative
/// ImportFrom to its absolute dotted name (TooManyLevelsError -> unchanged).
pub fn importfrom_absolute_name(eng: &pyinfer::graph::Engine, node: GNode) -> String {
    let md = eng.md(node.m);
    let (modname, level) = match &md.tree.nodes[node.n.idx()].kind {
        NodeKind::ImportFrom { modname, level, .. } => (md.tree.s(*modname).to_string(), *level),
        _ => return String::new(),
    };
    match level {
        Some(l) if l > 0 => eng
            .relative_to_absolute_name(&md, &modname, Some(l))
            .unwrap_or(modname),
        _ => modname,
    }
}

/// sys.builtin_module_names on the pinned interpreter (darwin CPython
/// 3.12.12) — get_module_part's relative-import shortcut
static BUILTIN_MODULES: &[&str] = &[
    "_abc", "_ast", "_asyncio", "_bisect", "_blake2", "_bz2", "_codecs",
    "_codecs_cn", "_codecs_hk", "_codecs_iso2022", "_codecs_jp", "_codecs_kr",
    "_codecs_tw", "_collections", "_contextvars", "_csv", "_ctypes", "_curses",
    "_curses_panel", "_datetime", "_decimal", "_elementtree", "_functools",
    "_hashlib", "_heapq", "_imp", "_io", "_json", "_locale", "_lsprof", "_lzma",
    "_md5", "_multibytecodec", "_multiprocessing", "_opcode", "_operator",
    "_pickle", "_posixshmem", "_posixsubprocess", "_queue", "_random",
    "_scproxy", "_sha1", "_sha2", "_sha3", "_signal", "_socket", "_sqlite3",
    "_sre", "_ssl", "_stat", "_statistics", "_string", "_struct", "_symtable",
    "_testbuffer", "_testimportmultiple", "_testinternalcapi", "_testmultiphase",
    "_testsinglephase", "_thread", "_tokenize", "_tracemalloc", "_typing",
    "_uuid", "_warnings", "_weakref", "_xxinterpchannels", "_xxsubinterpreters",
    "_xxtestfuzz", "_zoneinfo", "array", "atexit", "audioop", "binascii",
    "builtins", "cmath", "errno", "faulthandler", "fcntl", "gc", "grp",
    "itertools", "marshal", "math", "mmap", "posix", "pwd", "pyexpat",
    "readline", "resource", "select", "sys", "syslog", "termios", "time",
    "unicodedata", "xxsubtype", "zlib",
];

/// astroid modutils.get_module_part (modutils.py:384-441).
/// None == ImportError (caller keeps the dotted name unchanged).
fn get_module_part(
    eng: &pyinfer::graph::Engine,
    dotted_name: &str,
    context_relative: bool,
) -> Option<String> {
    if dotted_name.starts_with("os.path") {
        return Some("os.path".to_string());
    }
    let parts: Vec<&str> = dotted_name.split('.').collect();
    if context_relative && BUILTIN_MODULES.contains(&parts[0]) {
        if parts.len() > 2 {
            return None; // raise ImportError
        }
        return Some(parts[0].to_string());
    }
    for i in 0..parts.len() {
        if !eng.modutils_can_resolve(&parts[..i + 1]) {
            if i < std::cmp::max(1, parts.len().saturating_sub(2)) {
                return None; // raise ImportError
            }
            return Some(parts[..i].join("."));
        }
    }
    Some(dotted_name.to_string())
}

/// astroid modutils.is_stdlib_module
fn is_stdlib_module(eng: &pyinfer::graph::Engine, modname: &str) -> bool {
    let first = modname.split('.').next().unwrap_or(modname);
    eng.env.stdlib_module_names.iter().any(|m| m == first)
}

/// _get_first_import (imports.py:85-137). Returns (first, msgid)
fn get_first_import(
    eng: &pyinfer::graph::Engine,
    node: GNode,
    context: GNode,
    name: &str,
    base: Option<&str>,
    level: Option<u32>,
    alias: Option<&str>,
) -> (Option<GNode>, Option<&'static str>) {
    let fullname = match base {
        Some(b) if !b.is_empty() => format!("{b}.{name}"),
        _ => name.to_string(),
    };
    let body: Vec<pyast::NodeId> = {
        let md = eng.md(context.m);
        match &md.tree.nodes[context.n.idx()].kind {
            NodeKind::Module(d) => d.body.clone(),
            NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => d.body.clone(),
            NodeKind::ClassDef(d) => d.body.clone(),
            _ => Vec::new(),
        }
    };
    let mut found = false;
    let mut msg: &'static str = "W0404"; // "reimported"
    let mut first_found: Option<GNode> = None;
    let node_line = eng.fromlineno(node);
    let node_scope = eng.scope(node);
    'outer: for &b in &body {
        let first = GNode { m: context.m, n: b };
        if first == node {
            continue;
        }
        if eng.scope(first) == node_scope && eng.fromlineno(first) > node_line {
            continue;
        }
        let md = eng.md(first.m);
        match &md.tree.nodes[first.n.idx()].kind {
            NodeKind::Import { names } => {
                if names.iter().any(|&(n, _)| md.tree.s(n) == fullname) {
                    found = true;
                    first_found = Some(first);
                    break 'outer;
                }
                for &(iname, ialias) in names {
                    if ialias.is_none() && Some(md.tree.s(iname)) == alias {
                        found = true;
                        msg = "W0416"; // "shadowed-import"
                        first_found = Some(first);
                        break 'outer;
                    }
                }
            }
            NodeKind::ImportFrom { modname, names, level: flevel } => {
                if level == *flevel {
                    let fmod = md.tree.s(*modname);
                    for &(iname, ialias) in names {
                        let iname_s = md.tree.s(iname);
                        if fullname == format!("{fmod}.{iname_s}") {
                            found = true;
                            first_found = Some(first);
                            break 'outer;
                        }
                        if name != "*"
                            && name == iname_s
                            && alias.is_none()
                            && ialias.is_none()
                        {
                            found = true;
                            first_found = Some(first);
                            break 'outer;
                        }
                        if ialias.is_none() && Some(iname_s) == alias {
                            found = true;
                            msg = "W0416";
                            first_found = Some(first);
                            break 'outer;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if found {
        if let Some(first) = first_found {
            if !u::are_exclusive(eng, first, node) {
                return (Some(first), Some(msg));
            }
        }
    }
    (None, None)
}
