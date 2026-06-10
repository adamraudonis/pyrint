//! Lint pipeline orchestration: the Rust counterpart of PyLinter.check
//! (pylinter.py:672-727) for the `prylint . -E --disable=...` invocation.
//!
//! Phase 1 (`_get_asts`, pylinter.py:729-759): parse ALL files (rayon
//! parallel, results in file order); parse-phase messages (E0001/F0010/F0002)
//! print before any check-phase message, in file order. Ruff-rejected files
//! are re-judged by a single batched syntax-oracle subprocess.
//!
//! Phase 2 (`_lint_files`, pylinter.py:771-830): per file in order:
//! tokenize-form E0001 -> pragmas -> skip-file -> raw checkers (unicode) ->
//! AST walk (placeholder; statement counting only). Parallelized with rayon,
//! messages buffered per file and flushed strictly in file order.

use std::io::BufWriter;

use rayon::prelude::*;

use pyast::parse::TokEvent;
use pyast::source::{decode_source, DecodeError, SourceFile};
use pyast::tree::{NodeId, NodeKind, Tree};
use pycheckers::msgstore::{self, store, ResolveError};

use crate::discover::{self, FileItem};
use crate::msgstate::{is_message_enabled, process_tokens, FileState, GlobalState};
use crate::oracle::{self, Verdict};
use crate::reporter::{OutMsg, Reporter};

pub struct RunOpts {
    pub paths: Vec<String>,
    /// --disable values, comma-split, in CLI order
    pub disables: Vec<String>,
    /// -E/--errors-only. The baked global state (msgs.rs `enabled`) already
    /// assumes error mode; the flag is accepted and recorded for the future
    /// non-error-mode path.
    #[allow(dead_code)]
    pub errors_only: bool,
}

struct ParsedFile {
    tree: Tree,
    src: SourceFile,
    tokens: Vec<TokEvent>,
    read_path: String,
    abspath: String,
}

enum FileData {
    Parsed(Box<ParsedFile>),
    /// phase-1 message: (msgid, line-as-displayed, col, text)
    Phase1 { msgid: &'static str, line: i64, col: i64, text: String },
    /// ruff rejected; awaiting oracle verdict
    NeedsOracle,
}

enum Phase2Plan {
    Tree(Box<ParsedFile>),
    /// pylint's tokenize raised TokenError (E0001 tokenize form, no prefix)
    TokenizeErr { line: i64, col: i64, msg: String },
    /// astroid parses + tokenizes but ruff rejected: pylint would lint it;
    /// we cannot (no tree). Counts as linted, emits nothing.
    Untracked,
}

#[derive(Default, Clone, Copy)]
struct Stats {
    fatal: u64,
    error: u64,
    warning: u64,
    refactor: u64,
    convention: u64,
    info: u64,
    statements: u64,
}

impl Stats {
    fn count(&mut self, msgid: &str) {
        match msgid.chars().next().unwrap_or('I') {
            'F' => self.fatal += 1,
            'E' => self.error += 1,
            'W' => self.warning += 1,
            'R' => self.refactor += 1,
            'C' => self.convention += 1,
            _ => self.info += 1,
        }
    }
}

struct ModuleOut {
    msgs: Vec<OutMsg>,
    stats: Stats,
    linted: bool,
}

pub fn run(opts: &RunOpts) -> i32 {
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    // BaseReporter.path_strip_prefix = os.getcwd() + os.sep (base_reporter.py:37)
    let strip_prefix = format!("{cwd}/");

    // ---- config phase ------------------------------------------------
    let mut global = GlobalState::for_target_flags();
    // unknown/deleted --disable values are stashed and printed under module
    // "Command line" BEFORE -E folds W/R away (config_initialization.py:128
    // runs before _parse_error_mode at :145 — empirically confirmed,
    // notes/02 §18.5)
    let mut stashed: Vec<(&'static str, String)> = Vec::new();
    for item in &opts.disables {
        match global.cli_disable(item) {
            Ok(()) => {}
            Err(ResolveError::Unknown) => stashed.push((
                "W0012",
                format!(
                    "Unknown option value for '--disable', expected a valid pylint message and got '{item}'"
                ),
            )),
            Err(ResolveError::Deleted(s)) | Err(ResolveError::Moved(s)) => {
                stashed.push(("R0022", format!("Useless option value for '--disable', {s}")))
            }
        }
    }

    let stdout = std::io::stdout();
    let mut reporter = Reporter::new(BufWriter::new(stdout.lock()));

    for (msgid, text) in &stashed {
        // suppressed only by an explicit --disable of the message itself
        // (the -E category disable has not run yet at this point)
        let explicit = opts.disables.iter().any(|d| {
            d == msgid
                || (*msgid == "W0012" && d == "unknown-option-value")
                || (*msgid == "R0022" && d == "useless-option-value")
        });
        if explicit {
            continue;
        }
        let idx = store().by_msgid[msgid];
        reporter.handle(&OutMsg {
            module: "Command line".to_string(),
            path: "Command line".to_string(),
            line: 1,
            col: 0,
            msgid,
            symbol: msgstore::def(idx).symbol,
            text: text.clone(),
        });
        // config-phase messages set msg_status but their stats are zeroed by
        // PyLinter.open() before checking starts (linterstats.py:328-335)
    }

    // ---- discovery ----------------------------------------------------
    let cfg = discover::DiscoverConfig::default();
    let items = discover::expand_modules_fs(&opts.paths, &cfg);

    // ---- phase 1: parse all files (rayon, results in file order) ------
    let mut data: Vec<FileData> = items.par_iter().map(parse_one).collect();

    // batched oracle pass for ruff-rejected files
    let oracle_idx: Vec<usize> = data
        .iter()
        .enumerate()
        .filter(|(_, d)| matches!(d, FileData::NeedsOracle))
        .map(|(i, _)| i)
        .collect();
    let requests: Vec<(String, String)> = oracle_idx
        .iter()
        .map(|&i| (items[i].filepath.clone(), items[i].name.clone()))
        .collect();
    let verdicts = oracle::run_oracle(&requests);
    let mut phase2_extra: Vec<(usize, Phase2Plan)> = Vec::new();
    for (&i, verdict) in oracle_idx.iter().zip(verdicts.into_iter()) {
        match verdict {
            Verdict::SyntaxError { line, offset, msg } => {
                data[i] = FileData::Phase1 {
                    msgid: "E0001",
                    // `line or 1` / `col_offset or 0` (pylinter.py:1277-1278)
                    line: if line == 0 { 1 } else { line },
                    col: offset.unwrap_or(0),
                    text: msg,
                };
            }
            Verdict::ParseError { msg } => {
                data[i] = FileData::Phase1 {
                    msgid: "F0010",
                    line: 1,
                    col: 0,
                    text: format!("error while code parsing: {msg}"),
                };
            }
            Verdict::AstroidError => {
                // F0002 astroid-error: "%s: %s" with the crash-report message
                // (lint/utils.py:107-112). The crash file path is wall-clock
                // dependent; we synthesize the same format without writing it.
                let p = &items[i].filepath;
                data[i] = FileData::Phase1 {
                    msgid: "F0002",
                    line: 1,
                    col: 0,
                    text: format!(
                        "{p}: Fatal error while checking '{p}'. Please open an issue in our bug tracker so we address this. There is a pre-filled template that you can use in 'pylint-crash.txt'."
                    ),
                };
            }
            Verdict::Ok { tokenize: Some(t) } => {
                phase2_extra.push((i, Phase2Plan::TokenizeErr { line: t.line, col: t.col, msg: t.msg }));
            }
            Verdict::Ok { tokenize: None } => {
                eprintln!(
                    "prylint: {} parses with CPython but not with ruff; module skipped",
                    items[i].filepath
                );
                phase2_extra.push((i, Phase2Plan::Untracked));
            }
        }
    }

    // ---- flush phase-1 messages in file order --------------------------
    let mut stats = Stats::default();
    let mut phase2: Vec<(usize, Phase2Plan)> = Vec::new();
    for (i, d) in data.into_iter().enumerate() {
        match d {
            FileData::Phase1 { msgid, line, col, text } => {
                let idx = store().by_msgid[msgid];
                // base FileState during phase 1: global state only
                if !is_message_enabled(&global, None, idx, Some(line.max(0) as u32)) {
                    continue;
                }
                stats.count(msgid);
                reporter.handle(&OutMsg {
                    module: items[i].name.clone(),
                    // node-less: abspath = current_file = FileItem.filepath
                    // as discovered (relative): cwd strip is a no-op
                    path: items[i].filepath.replacen(&strip_prefix, "", 1),
                    line,
                    col,
                    msgid,
                    symbol: msgstore::def(idx).symbol,
                    text,
                });
            }
            FileData::Parsed(p) => phase2.push((i, Phase2Plan::Tree(p))),
            FileData::NeedsOracle => {} // replaced above
        }
    }
    phase2.extend(phase2_extra);
    phase2.sort_by_key(|(i, _)| *i);

    // ---- phase 2: per-module checks (rayon, ordered flush) ------------
    // prepare_checkers (pylinter.py:588-598): the unicode raw checker is
    // kept iff any of its messages is enabled package-wise
    let unicode_prepared = ["E2501", "E2502", "C2503", "E2510", "E2511", "E2512", "E2513", "E2514", "E2515"]
        .iter()
        .any(|m| global.enabled(store().by_msgid[m]));

    let results: Vec<ModuleOut> = phase2
        .par_iter()
        .map(|(i, plan)| lint_one(&items[*i], plan, &global, &strip_prefix, unicode_prepared))
        .collect();

    let mut any_linted = false;
    for r in &results {
        any_linted |= r.linted;
        stats.fatal += r.stats.fatal;
        stats.error += r.stats.error;
        stats.warning += r.stats.warning;
        stats.refactor += r.stats.refactor;
        stats.convention += r.stats.convention;
        stats.info += r.stats.info;
        stats.statements += r.stats.statements;
        for m in &r.msgs {
            reporter.handle(m);
        }
    }
    reporter.flush();

    // ---- exit code (run.py:245-260 + _report_evaluation) ---------------
    let msg_status = reporter.msg_status;
    let score: Option<f64> = if !any_linted || stats.statements == 0 {
        None
    } else if stats.fatal > 0 {
        Some(0.0)
    } else {
        let penalty = (5 * stats.error + stats.warning + stats.refactor + stats.convention) as f64;
        Some((10.0 - penalty / stats.statements as f64 * 10.0).max(0.0))
    };
    match score {
        None => msg_status,
        Some(s) if s >= 10.0 => 0,
        Some(_) => {
            if msg_status != 0 {
                msg_status
            } else {
                1
            }
        }
    }
}

/// Phase-1 work for one file: read, decode, parse (pylinter.get_ast /
/// astroid file_build error taxonomy, builder.py:113-149).
fn parse_one(item: &FileItem) -> FileData {
    // modutils.get_source_file: a .pyi argument resolves to the sibling .py
    // source when it exists (PY_SOURCE_EXTS order, prefer_stubs=False)
    let mut read_path = item.filepath.clone();
    if let Some(base) = item.filepath.strip_suffix(".pyi") {
        let py = format!("{base}.py");
        if std::path::Path::new(&py).exists() {
            read_path = py;
        }
    }
    // error messages embed os.path.abspath of the resolved source path
    let abspath = discover::absolute(&read_path);
    let bytes = match std::fs::read(&read_path) {
        Ok(b) => b,
        Err(e) => {
            // OSError in open_source_file -> AstroidBuildingError ->
            // F0010 "Unable to load file {path}:\n{error}" (builder.py:119-125)
            return FileData::Phase1 {
                msgid: "F0010",
                line: 1,
                col: 0,
                text: format!("error while code parsing: Unable to load file {abspath}:\n{e}"),
            };
        }
    };
    let src = match decode_source(&bytes, &abspath) {
        Ok(src) => src,
        // SyntaxError/LookupError from detect_encoding -> AstroidSyntaxError
        // -> E0001 (lineno None -> 0 -> displayed 1; offset None -> 0)
        Err(DecodeError::Syntax(msg)) | Err(DecodeError::Lookup(msg)) => {
            return FileData::Phase1 {
                msgid: "E0001",
                line: 1,
                col: 0,
                text: format!("Parsing failed: '{msg}'"),
            };
        }
        // UnicodeError -> AstroidBuildingError -> F0010 (builder.py:135-139)
        Err(DecodeError::Unicode) => {
            return FileData::Phase1 {
                msgid: "F0010",
                line: 1,
                col: 0,
                text: format!("error while code parsing: Wrong or no encoding specified for {abspath}."),
            };
        }
    };
    // astroid _data_build modname handling (builder.py:200-208)
    let (modname, package) = if let Some(stripped) = item.name.strip_suffix(".__init__") {
        (stripped.to_string(), true)
    } else {
        let stem_is_init = std::path::Path::new(&read_path)
            .file_stem()
            .map(|s| s == "__init__")
            .unwrap_or(false);
        (item.name.clone(), stem_is_init)
    };
    let outcome = pyast::parse::parse_module(&src, &modname, &abspath, package);
    match outcome.tree {
        Some(tree) => FileData::Parsed(Box::new(ParsedFile {
            tree,
            src,
            tokens: outcome.tokens,
            read_path,
            abspath,
        })),
        None => FileData::NeedsOracle,
    }
}

/// Phase-2 work for one module (`_lint_file` + `_check_astroid_module`,
/// pylinter.py:798-830 / 1062-1106).
fn lint_one(
    item: &FileItem,
    plan: &Phase2Plan,
    global: &GlobalState,
    strip_prefix: &str,
    unicode_prepared: bool,
) -> ModuleOut {
    let mut out = ModuleOut { msgs: Vec::new(), stats: Stats::default(), linted: true };
    match plan {
        Phase2Plan::Untracked => out,
        Phase2Plan::TokenizeErr { line, col, msg } => {
            // tokenize.TokenError -> E0001 with args = ex.args[0] verbatim
            // (NO "Parsing failed:" prefix), line/col from ex.args[1]
            // (pylinter.py:1080-1090); module skipped afterwards
            let idx = store().by_msgid["E0001"];
            // fresh FileState exists in pylint here, but with no pragmas
            // processed the lookup reduces to global state
            if is_message_enabled(global, None, idx, Some((*line).max(0) as u32)) {
                out.stats.count("E0001");
                out.msgs.push(OutMsg {
                    module: item.name.clone(),
                    // current_file was reset to module.file (absolute)
                    path: discover::absolute(&item.filepath).replacen(strip_prefix, "", 1),
                    line: if *line == 0 { 1 } else { *line },
                    col: *col,
                    msgid: "E0001",
                    symbol: "syntax-error",
                    text: msg.clone(),
                });
            }
            out
        }
        Phase2Plan::Tree(p) => {
            let mut fs = FileState::new(&p.tree);
            let module = item.name.clone();
            let path = p.abspath.replacen(strip_prefix, "", 1);
            let mut msgs: Vec<OutMsg> = Vec::new();
            let mut stats = Stats::default();
            {
                let mut add = |fs: &mut FileState, msgid: &str, line: u32, col: i64, text: String| {
                    let Some(&idx) = store().by_msgid.get(msgid) else { return };
                    if !is_message_enabled(global, Some(fs), idx, Some(line)) {
                        return;
                    }
                    stats.count(msgid);
                    msgs.push(OutMsg {
                        module: module.clone(),
                        path: path.clone(),
                        line: if line == 0 { 1 } else { line as i64 },
                        col,
                        msgid: msgstore::def(idx).msgid,
                        symbol: msgstore::def(idx).symbol,
                        text,
                    });
                };
                // 1. pragmas (PyLinter.process_tokens); skip-file aborts
                //    before raw checkers and the walker
                let mut pragma_sink = |fs: &mut FileState, msgid: &str, line: u32, text: String| {
                    add(fs, msgid, line, 0, text);
                };
                let ignore_file = process_tokens(&p.tokens, &p.src, &mut fs, &mut pragma_sink);
                if !ignore_file {
                    // 2. raw checkers in prepared order (unicode only)
                    if unicode_prepared {
                        if let Ok(bytes) = std::fs::read(&p.read_path) {
                            pycheckers::unicode::process_module(&bytes, &mut |um| {
                                let text = store()
                                    .by_msgid
                                    .get(um.msgid)
                                    .map(|&i| msgstore::def(i).template.to_string())
                                    .unwrap_or_default();
                                add(&mut fs, um.msgid, um.line, um.col as i64, text);
                            });
                        }
                    }
                    // 3. AST walk — no checkers registered yet; the walker
                    //    still accumulates nbstatements (ast_walker.py:79-80)
                    stats.statements += count_statements(&p.tree);
                }
            }
            out.msgs = msgs;
            out.stats = stats;
            out
        }
    }
}

/// astroid `is_statement` node kinds (subclasses of _base_nodes.Statement).
fn is_statement_kind(k: &NodeKind) -> bool {
    matches!(
        k,
        NodeKind::FunctionDef(_)
            | NodeKind::AsyncFunctionDef(_)
            | NodeKind::ClassDef(_)
            | NodeKind::Return { .. }
            | NodeKind::Delete { .. }
            | NodeKind::Assign { .. }
            | NodeKind::AugAssign { .. }
            | NodeKind::AnnAssign { .. }
            | NodeKind::TypeAlias { .. }
            | NodeKind::For(_)
            | NodeKind::AsyncFor(_)
            | NodeKind::While { .. }
            | NodeKind::If { .. }
            | NodeKind::With(_)
            | NodeKind::AsyncWith(_)
            | NodeKind::Match { .. }
            | NodeKind::Raise { .. }
            | NodeKind::Try(_)
            | NodeKind::TryStar(_)
            | NodeKind::Assert { .. }
            | NodeKind::Import { .. }
            | NodeKind::ImportFrom { .. }
            | NodeKind::Global { .. }
            | NodeKind::Nonlocal { .. }
            | NodeKind::Expr { .. }
            | NodeKind::Pass
            | NodeKind::Break
            | NodeKind::Continue
            | NodeKind::ExceptHandler { .. }
    )
}

fn count_statements(tree: &Tree) -> u64 {
    let mut count = 0u64;
    let mut stack: Vec<NodeId> = Vec::new();
    tree.push_children(NodeId::MODULE, &mut stack);
    while let Some(id) = stack.pop() {
        if is_statement_kind(tree.kind(id)) {
            count += 1;
        }
        tree.push_children(id, &mut stack);
    }
    count
}
